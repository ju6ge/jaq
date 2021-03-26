use crate::filter::{Filter, NewFilter, Ref};
use crate::functions::NewFunc;
use crate::ops::{LogicOp, MathOp};
use crate::path::{Path, PathElem};
use crate::val::Atom;
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::convert::{TryFrom, TryInto};
use pest::iterators::{Pair, Pairs};
use pest::prec_climber::PrecClimber;
use pest::Parser;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct FilterParser;

impl Filter {
    pub fn parse(s: &str) -> Result<Self, pest::error::Error<Rule>> {
        Ok(Self::from(FilterParser::parse(Rule::main, s)?))
    }
}

lazy_static::lazy_static! {
    static ref PREC_CLIMBER: PrecClimber<Rule> = {
        use Rule::*;
        use pest::prec_climber::{Operator, Assoc::*};

        PrecClimber::new(Vec::from([
            Operator::new(pipe, Left),
            Operator::new(comma, Left),
            Operator::new(assign, Right) | Operator::new(update, Right) | Operator::new(update_with, Right),
            Operator::new(or, Left),
            Operator::new(and, Left),
            Operator::new(eq, Left) | Operator::new(ne, Left),
            Operator::new(gt, Left) | Operator::new(ge, Left) | Operator::new(lt, Left) | Operator::new(le, Left),
            Operator::new(add, Left) | Operator::new(sub, Left),
            Operator::new(mul, Left) | Operator::new(div, Left),
            Operator::new(rem, Left)
        ]))
    };
}

impl From<Pairs<'_, Rule>> for Filter {
    fn from(pairs: Pairs<Rule>) -> Self {
        PREC_CLIMBER.climb(
            pairs,
            |pair: Pair<Rule>| Self::from(pair),
            |lhs: Self, op: Pair<Rule>, rhs: Self| {
                let lhs = Box::new(lhs);
                let rhs = Box::new(rhs);
                match op.as_rule() {
                    // TODO: make this nicer
                    Rule::assign => Self::Ref(Ref::Assign((*lhs).try_into().unwrap(), rhs)),
                    Rule::update => Self::Ref(Ref::Update((*lhs).try_into().unwrap(), rhs)),
                    Rule::update_with => {
                        let op = op.into_inner().next().unwrap();
                        let op = MathOp::try_from(op.as_rule()).unwrap();
                        let id = Box::new(Self::Ref(Ref::identity()));
                        let f = Box::new(Self::New(NewFilter::Math(id, op, rhs)));
                        Self::Ref(Ref::Update((*lhs).try_into().unwrap(), f))
                    }
                    Rule::pipe => Self::Ref(Ref::Pipe(lhs, rhs)),
                    Rule::comma => Self::Ref(Ref::Comma(lhs, rhs)),
                    rule => {
                        if let Ok(op) = LogicOp::try_from(rule) {
                            Self::New(NewFilter::Logic(lhs, op, rhs))
                        } else if let Ok(op) = MathOp::try_from(rule) {
                            Self::New(NewFilter::Math(lhs, op, rhs))
                        } else {
                            unreachable!()
                        }
                    }
                }
            },
        )
    }
}

impl TryFrom<Filter> for Path {
    type Error = ();

    fn try_from(f: Filter) -> Result<Self, Self::Error> {
        match f {
            Filter::Ref(Ref::Path(p)) => Ok(p),
            _ => Err(()),
        }
    }
}

impl From<Pair<'_, Rule>> for Filter {
    fn from(pair: Pair<Rule>) -> Self {
        let rule = pair.as_rule();
        let mut inner = pair.into_inner();
        match rule {
            Rule::expr => Self::from(inner),
            Rule::atom => Self::New(NewFilter::Atom(inner.next().unwrap().into())),
            Rule::array => {
                let contents = if inner.peek().is_none() {
                    Self::Ref(Ref::Empty)
                } else {
                    Self::from(inner)
                };
                Self::New(NewFilter::Array(Box::new(contents)))
            }
            Rule::object => {
                let contents = inner.map(|kv| {
                    let mut iter = kv.into_inner();
                    let key = iter.next().unwrap();
                    let key = match key.as_rule() {
                        Rule::identifier => Atom::Str(key.as_str().to_string()).into(),
                        Rule::string => Atom::from(key).into(),
                        Rule::expr => Self::from(key),
                        _ => unreachable!(),
                    };
                    let value = match iter.next() {
                        Some(value) => Self::from(value),
                        None => todo!(),
                    };
                    assert_eq!(iter.next(), None);
                    (key, value)
                });
                Self::New(NewFilter::Object(contents.collect()))
            }
            Rule::ite => {
                let mut ite = inner.map(|p| Box::new(Self::from(p)));
                let cond = ite.next().unwrap();
                let truth = ite.next().unwrap();
                let falsity = ite.next().unwrap();
                assert!(ite.next().is_none());
                Self::Ref(Ref::IfThenElse(cond, truth, falsity))
            }
            Rule::function => {
                let name = inner.next().unwrap().as_str();
                let args = match inner.next() {
                    None => Box::new(core::iter::empty()) as Box<dyn Iterator<Item = _>>,
                    Some(args) => Box::new(args.into_inner().map(Self::from)),
                };
                assert_eq!(inner.next(), None);
                Self::try_from(name, args).unwrap()
            }
            Rule::path => Self::Ref(Ref::Path(Path::new(inner.flat_map(PathElem::from_path)))),
            _ => unreachable!(),
        }
    }
}

impl From<Pair<'_, Rule>> for Atom {
    fn from(pair: Pair<Rule>) -> Self {
        use serde_json::Number;
        match pair.as_rule() {
            Rule::null => Self::Null,
            Rule::boole => Self::Bool(pair.as_str().parse::<bool>().unwrap()),
            Rule::number => Self::Num(pair.as_str().parse::<Number>().unwrap().try_into().unwrap()),
            Rule::string => Self::Str(pair.into_inner().next().unwrap().as_str().to_string()),
            _ => unreachable!(),
        }
    }
}

impl PathElem<Filter> {
    fn from_path(pair: Pair<Rule>) -> impl Iterator<Item = Self> + '_ {
        use core::iter::{empty, once};
        let mut iter = pair.into_inner();
        let index = iter.next().unwrap();
        let index = match index.as_rule() {
            Rule::path_index => {
                let index = Self::from_index(index).0.to_string();
                Box::new(once(Self::Index(Atom::Str(index).into())))
            }
            // just a dot
            _ => Box::new(empty()) as Box<dyn Iterator<Item = _>>,
        };
        index.chain(iter.map(PathElem::from_range))
    }

    fn from_index(pair: Pair<Rule>) -> (&str, bool) {
        let mut iter = pair.into_inner();
        let index = iter.next().unwrap().into_inner().next().unwrap().as_str();
        let question = iter.next().is_some();
        assert_eq!(iter.next(), None);
        (index, question)
    }

    fn from_range(pair: Pair<Rule>) -> PathElem<Filter> {
        //println!("range: {:?}", pair.as_rule());
        match pair.into_inner().next() {
            None => Self::Range(None, None),
            Some(range) => match range.as_rule() {
                Rule::at => Self::Index(Filter::from(range.into_inner())),
                Rule::from => Self::Range(Some(Filter::from(range.into_inner())), None),
                Rule::until => Self::Range(None, Some(Filter::from(range.into_inner()))),
                Rule::from_until => {
                    let mut iter = range.into_inner().map(|r| Some(Filter::from(r)));
                    let from = iter.next().unwrap();
                    let until = iter.next().unwrap();
                    assert!(iter.next().is_none());
                    Self::Range(from, until)
                }
                _ => unreachable!(),
            },
        }
    }
}

impl TryFrom<Rule> for LogicOp {
    type Error = ();
    fn try_from(rule: Rule) -> Result<Self, Self::Error> {
        match rule {
            Rule::or => Ok(LogicOp::Or),
            Rule::and => Ok(LogicOp::And),
            Rule::eq => Ok(LogicOp::Eq),
            Rule::ne => Ok(LogicOp::Ne),
            Rule::gt => Ok(LogicOp::Gt),
            Rule::ge => Ok(LogicOp::Ge),
            Rule::lt => Ok(LogicOp::Lt),
            Rule::le => Ok(LogicOp::Le),
            _ => Err(()),
        }
    }
}

impl TryFrom<Rule> for MathOp {
    type Error = ();
    fn try_from(rule: Rule) -> Result<Self, Self::Error> {
        match rule {
            Rule::add => Ok(MathOp::Add),
            Rule::sub => Ok(MathOp::Sub),
            Rule::mul => Ok(MathOp::Mul),
            Rule::div => Ok(MathOp::Div),
            Rule::rem => Ok(MathOp::Rem),
            _ => Err(()),
        }
    }
}

impl TryFrom<(&str, [Box<Filter>; 0])> for Filter {
    type Error = ();
    fn try_from((name, []): (&str, [Box<Filter>; 0])) -> Result<Self, ()> {
        match name {
            "empty" => Ok(Self::Ref(Ref::Empty)),
            "any" => Ok(Self::New(NewFilter::Function(NewFunc::Any))),
            "all" => Ok(Self::New(NewFilter::Function(NewFunc::All))),
            "not" => Ok(Self::New(NewFilter::Function(NewFunc::Not))),
            "length" => Ok(Self::New(NewFilter::Function(NewFunc::Length))),
            "type" => Ok(Self::New(NewFilter::Function(NewFunc::Type))),
            "add" => Ok(Self::New(NewFilter::Function(NewFunc::Add))),
            _ => Err(()),
        }
    }
}

impl TryFrom<(&str, [Box<Filter>; 1])> for Filter {
    type Error = ();
    fn try_from((name, [arg1]): (&str, [Box<Filter>; 1])) -> Result<Self, ()> {
        match name {
            "first" => Ok(Self::Ref(Ref::First(arg1))),
            "last" => Ok(Self::Ref(Ref::Last(arg1))),
            "map" => Ok(Self::New(NewFilter::Function(NewFunc::Map(arg1)))),
            "select" => Ok(Self::Ref(Ref::select(arg1))),
            "recurse" => Ok(Self::Ref(Ref::Recurse(arg1))),
            _ => Err(()),
        }
    }
}

impl TryFrom<(&str, [Box<Filter>; 2])> for Filter {
    type Error = ();
    fn try_from((name, [arg1, arg2]): (&str, [Box<Filter>; 2])) -> Result<Self, ()> {
        match name {
            "limit" => Ok(Self::Ref(Ref::Limit(arg1, arg2))),
            _ => Err(()),
        }
    }
}

impl Filter {
    fn try_from(name: &str, args: impl Iterator<Item = Filter>) -> Option<Self> {
        let mut args = args.map(Box::new);
        if let Some(arg1) = args.next() {
            // unary or higher-arity function
            if let Some(arg2) = args.next() {
                // binary or higher-arity function
                if let Some(arg3) = args.next() {
                    // ternary or higher-arity function
                    if let Some(_arg4) = args.next() {
                        // quaternary or higher-arity function
                        None
                    } else {
                        // ternary function
                        match name {
                            "fold" => Some(Self::Ref(Ref::Fold(arg1, arg2, arg3))),
                            _ => None,
                        }
                    }
                } else {
                    // binary function
                    (name, [arg1, arg2]).try_into().ok()
                }
            } else {
                // unary function
                (name, [arg1]).try_into().ok()
            }
        } else {
            // nullary function
            (name, []).try_into().ok()
        }
    }
}
