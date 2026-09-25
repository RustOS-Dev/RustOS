//! Shell syntax tree.

use alloc::rc::Rc;
use core::cell::RefCell;
use rustos_rt::prelude::*;

/// A word before expansion: a sequence of parts with quoting information.
#[derive(Debug, Clone, PartialEq)]
pub enum WordPart {
    /// Unquoted literal text (subject to globbing and field splitting of
    /// expansion results).
    Lit(String),
    /// Quoted literal text.
    Quoted(String),
    /// `$name` / `${...}`; bool = inside double quotes.
    Param(String, bool),
    /// `$(...)` or backticks; bool = inside double quotes.
    CmdSub(String, bool),
    /// `$((...))`
    Arith(String),
    /// Leading `~` or `~user`.
    Tilde(String),
}

pub type Word = Vec<WordPart>;

#[derive(Debug, Clone, PartialEq)]
pub enum RedirKind {
    In,
    Out,
    Append,
    /// `n>&m` / `n<&m`
    Dup,
    /// Here-document (body already collected); bool = expand variables.
    Here(Rc<RefCell<String>>, bool),
    /// `<>`
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Redir {
    pub fd: i32,
    pub kind: RedirKind,
    pub target: Word,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Simple {
        assigns: Vec<(String, Word)>,
        words: Vec<Word>,
        redirs: Vec<Redir>,
    },
    If {
        branches: Vec<(List, List)>,
        otherwise: Option<List>,
        redirs: Vec<Redir>,
    },
    While {
        cond: List,
        body: List,
        until: bool,
        redirs: Vec<Redir>,
    },
    For {
        var: String,
        items: Option<Vec<Word>>,
        body: List,
        redirs: Vec<Redir>,
    },
    Case {
        word: Word,
        arms: Vec<(Vec<Word>, List)>,
        redirs: Vec<Redir>,
    },
    Group(List, Vec<Redir>),
    Subshell(List, Vec<Redir>),
    FuncDef(String, Box<Command>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub negate: bool,
    pub cmds: Vec<Command>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Connector {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(Connector, Pipeline)>,
    pub background: bool,
    /// Source text (for job listings).
    pub text: String,
}

pub type List = Vec<AndOr>;
