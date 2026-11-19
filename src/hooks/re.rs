//! A small regular expression engine for the hooks: the extended regular expressions of grep and
//! awk that the carried shell hooks use, so that their patterns are ported as written instead of
//! being rewritten by hand. It supports literals, `.`, bracket expressions (ranges, negation and
//! `[:space:]`-style classes), groups, alternation, `*`, `+`, `?`, `^` and `$`, matched by
//! backtracking over characters. Inside a bracket a backslash escapes the next character, as in
//! awk; `\t` and `\n` are a tab and a newline everywhere.
//!
//! The hooks ask only whether a pattern matches, where the leftmost match starts and how long it
//! is, and replace every match. Each pattern is greedy and its alternatives start differently, so
//! the first match found by backtracking is the leftmost-longest one POSIX asks for.

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Vec<ClassItem>, bool),
    Start,
    End,
    Group(Box<Node>),
    Alt(Vec<Node>),
    Seq(Vec<Node>),
    Repeat(Box<Node>, usize, Option<usize>),
}

#[derive(Debug, Clone)]
enum ClassItem {
    One(char),
    Range(char, char),
    Space,
    Alpha,
    Digit,
    Alnum,
    Upper,
    Lower,
}

pub struct Re {
    node: Node,
    icase: bool,
}

thread_local! {
    /// Each pattern compiled once per thread: a hook checks the same patterns on every segment.
    static CACHE: std::cell::RefCell<std::collections::HashMap<(String, bool), std::rc::Rc<Re>>> =
        Default::default();
}

impl Re {
    pub fn new(pattern: &str) -> std::rc::Rc<Re> {
        Re::cached(pattern, false)
    }

    /// Case is ignored, as with `grep -i`.
    pub fn icase(pattern: &str) -> std::rc::Rc<Re> {
        Re::cached(pattern, true)
    }

    fn cached(pattern: &str, icase: bool) -> std::rc::Rc<Re> {
        CACHE.with(|c| {
            c.borrow_mut()
                .entry((pattern.to_string(), icase))
                .or_insert_with(|| std::rc::Rc::new(Re::build(pattern, icase)))
                .clone()
        })
    }

    fn build(pattern: &str, icase: bool) -> Re {
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = Parser { s: &chars, i: 0 };
        let node = p.alt();
        assert!(p.i == chars.len(), "bad pattern {pattern:?} at {}", p.i);
        Re { node, icase }
    }

    /// The leftmost match in `text`: its start and end, in characters.
    pub fn find_chars(&self, text: &[char]) -> Option<(usize, usize)> {
        self.find_from(text, 0)
    }

    fn find_from(&self, text: &[char], from: usize) -> Option<(usize, usize)> {
        for start in from..=text.len() {
            let mut end = None;
            if self.m(&self.node, text, start, &mut |e| {
                end = Some(e);
                true
            }) {
                return end.map(|e| (start, e));
            }
        }
        None
    }

    pub fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        self.find_chars(&chars).is_some()
    }

    /// True when any line of `text` matches, as `grep -q` reads its input.
    pub fn any_line(&self, text: &str) -> bool {
        text.split('\n').any(|l| self.is_match(l))
    }

    /// `text` with every match replaced by `with`, as awk's `gsub`.
    pub fn replace_all(&self, text: &str, with: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut at = 0;
        while at <= chars.len() {
            match self.find_from(&chars, at) {
                Some((s, e)) => {
                    out.extend(&chars[at..s]);
                    out.push_str(with);
                    if e == s {
                        if s < chars.len() {
                            out.push(chars[s]);
                        }
                        at = s + 1;
                    } else {
                        at = e;
                    }
                }
                None => {
                    out.extend(&chars[at..]);
                    break;
                }
            }
        }
        out
    }

    fn eq(&self, a: char, b: char) -> bool {
        a == b || (self.icase && a.to_lowercase().eq(b.to_lowercase()))
    }

    fn in_class(&self, items: &[ClassItem], c: char) -> bool {
        let test = |c: char| {
            items.iter().any(|it| match *it {
                ClassItem::One(x) => x == c,
                ClassItem::Range(a, b) => a <= c && c <= b,
                ClassItem::Space => matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'),
                ClassItem::Alpha => c.is_ascii_alphabetic(),
                ClassItem::Digit => c.is_ascii_digit(),
                ClassItem::Alnum => c.is_ascii_alphanumeric(),
                ClassItem::Upper => c.is_ascii_uppercase(),
                ClassItem::Lower => c.is_ascii_lowercase(),
            })
        };
        test(c) || (self.icase && (test(c.to_ascii_lowercase()) || test(c.to_ascii_uppercase())))
    }

    /// Match `node` at `i`, then call `k` with the end; true when `k` accepts.
    fn m(&self, node: &Node, t: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        match node {
            Node::Char(c) => i < t.len() && self.eq(t[i], *c) && k(i + 1),
            Node::Any => i < t.len() && k(i + 1),
            Node::Class(items, neg) => {
                i < t.len() && (self.in_class(items, t[i]) != *neg) && k(i + 1)
            }
            Node::Start => i == 0 && k(i),
            Node::End => i == t.len() && k(i),
            Node::Group(n) => self.m(n, t, i, k),
            Node::Alt(alts) => alts.iter().any(|a| self.m(a, t, i, k)),
            Node::Seq(items) => self.seq(items, t, i, k),
            Node::Repeat(n, min, max) => self.rep(n, *min, *max, 0, t, i, k),
        }
    }

    fn seq(&self, items: &[Node], t: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        match items.split_first() {
            None => k(i),
            Some((first, rest)) => self.m(first, t, i, &mut |j| self.seq(rest, t, j, k)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(
        &self,
        n: &Node,
        min: usize,
        max: Option<usize>,
        count: usize,
        t: &[char],
        i: usize,
        k: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        // Greedy: one more first (never an empty step), then stop if enough were taken.
        if max.is_none_or(|m| count < m)
            && self.m(n, t, i, &mut |j| {
                j > i && self.rep(n, min, max, count + 1, t, j, k)
            })
        {
            return true;
        }
        count >= min && k(i)
    }
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn alt(&mut self) -> Node {
        let mut alts = vec![self.seq()];
        while self.peek() == Some('|') {
            self.i += 1;
            alts.push(self.seq());
        }
        if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            Node::Alt(alts)
        }
    }

    fn seq(&mut self) -> Node {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom();
            let atom = match self.peek() {
                Some('*') => {
                    self.i += 1;
                    Node::Repeat(Box::new(atom), 0, None)
                }
                Some('+') => {
                    self.i += 1;
                    Node::Repeat(Box::new(atom), 1, None)
                }
                Some('?') => {
                    self.i += 1;
                    Node::Repeat(Box::new(atom), 0, Some(1))
                }
                _ => atom,
            };
            items.push(atom);
        }
        Node::Seq(items)
    }

    fn escaped(c: char) -> char {
        match c {
            't' => '\t',
            'n' => '\n',
            c => c,
        }
    }

    fn atom(&mut self) -> Node {
        let c = self.peek().unwrap();
        self.i += 1;
        match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let inner = self.alt();
                assert_eq!(self.peek(), Some(')'), "unclosed group");
                self.i += 1;
                Node::Group(Box::new(inner))
            }
            '[' => self.class(),
            '\\' => {
                let e = self.peek().expect("trailing backslash");
                self.i += 1;
                Node::Char(Self::escaped(e))
            }
            c => Node::Char(c),
        }
    }

    fn class(&mut self) -> Node {
        let mut neg = false;
        if self.peek() == Some('^') {
            neg = true;
            self.i += 1;
        }
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().expect("unclosed bracket");
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            if c == '[' && self.s.get(self.i + 1) == Some(&':') {
                let rest: String = self.s[self.i..].iter().collect();
                let end = rest.find(":]").expect("unclosed class name");
                let name = &rest[2..end];
                items.push(match name {
                    "space" => ClassItem::Space,
                    "alpha" => ClassItem::Alpha,
                    "digit" => ClassItem::Digit,
                    "alnum" => ClassItem::Alnum,
                    "upper" => ClassItem::Upper,
                    "lower" => ClassItem::Lower,
                    n => panic!("unknown class {n}"),
                });
                self.i += rest[..end + 2].chars().count();
                continue;
            }
            self.i += 1;
            let lo = if c == '\\' {
                let e = self.peek().expect("trailing backslash");
                self.i += 1;
                Self::escaped(e)
            } else {
                c
            };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|&n| n != ']') {
                self.i += 1;
                let mut hi = self.peek().unwrap();
                self.i += 1;
                if hi == '\\' {
                    hi = Self::escaped(self.peek().unwrap());
                    self.i += 1;
                }
                items.push(ClassItem::Range(lo, hi));
            } else {
                items.push(ClassItem::One(lo));
            }
        }
        Node::Class(items, neg)
    }
}

#[cfg(test)]
mod tests {
    use super::Re;

    #[test]
    fn the_patterns_of_the_hooks_match_as_grep_does() {
        let r = Re::new(r"(^|[[:space:]])git[[:space:]]+push([[:space:]]|$)");
        assert!(r.is_match("git push"));
        assert!(r.is_match("x git  push y"));
        assert!(!r.is_match("git pushx"));
        assert!(!r.is_match("legit push"));
        let r = Re::new(r"^[A-Za-z_][A-Za-z0-9_]*=");
        assert_eq!(
            r.find_chars(&"AB=c".chars().collect::<Vec<_>>()),
            Some((0, 3))
        );
        assert!(Re::icase("gh").is_match("GH pr"));
        assert_eq!(Re::new(r#"[\\'"]"#).replace_all("a'b\\\"c", ""), "abc");
        assert_eq!(Re::new(r"\$\([^()]*\)").replace_all("a $(b) c", ""), "a  c");
        assert!(Re::new(r"[]a]").is_match("]"));
        assert!(Re::new(r"-[a-zA-Z]*n[a-zA-Z]+([[:space:]]|$)").is_match("-anm"));
    }
}
