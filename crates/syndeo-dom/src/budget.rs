//! Parsing with a bound on the work a page can cause.
//!
//! html5ever is correct, and for ordinary pages fast, but several of its paths
//! cost more than the size of the input: every block start tag scans the
//! stack of open elements, so a page nested N deep costs N² (200,000 levels
//! took eleven minutes); a tag's attributes are checked for duplicates one
//! against another, so a tag with N attributes costs N²; a repeated `<html>`
//! merges its attributes into those already there; text foster-parented out
//! of a table searches its parent's children. Each is a page a hostile origin
//! can serve to hold `syndeo browse` or the agent for as long as it likes.
//!
//! So a page is parsed against a budget of work units, counted as html5ever
//! does the work rather than estimated from the input:
//!
//! - Every byte costs one: the lookahead reads it and so does the tokenizer.
//!   The lookahead is told how many bytes the budget has left and reads no
//!   further, so text with no tag in it, a comment, a script, a quoted value
//!   or a tag that never ends costs what it is read for, like anything else.
//! - Every token the tree builder is given costs one, plus one for each
//!   element it holds on its stack of open elements and its list of active
//!   formatting elements, which are what its per-token scans walk, plus one
//!   for each attribute the token carries. ([`Gate`])
//! - Every operation on the tree costs what it walks: inserting before a
//!   sibling or removing a node costs the number of its siblings, moving
//!   children costs their number, merging attributes costs those already
//!   there. ([`Metered`])
//! - A tag costs the square of its attribute count before the tokenizer is
//!   given it, because that is when the tokenizer pays. [`Lookahead`] follows
//!   the tokenizer's own states through the input to find where each tag
//!   ends and how many attributes it has, and the tokenizer is only ever
//!   given the input up to the end of the next tag.
//!
//! When the budget is spent, the rest of the page is not parsed: the
//! [`Document`](crate::Document) says so, and says how far it got. Nothing
//! is timed, so the same page is always cut at the same place.
//!
//! This bounds the work of parsing a body that has already arrived. How
//! large a body the network process accepts is a separate limit
//! (`max_body_bytes` in `syndeo-net`, 64 MiB by default); a body within that
//! may still be only partly parsed, and one parsed whole was not necessarily
//! the whole response.

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::{RawKind, ScriptEscapeKind};
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::tree_builder::{
    Attribute, ElementFlags, NodeOrText, QuirksMode, Tracer, TreeBuilder, TreeBuilderOpts, TreeSink,
};
use html5ever::TokenizerResult;
use html5ever::{LocalName, QualName};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The work a page may cost, in the units above. An ordinary page costs a
/// few million; an ordinary 16 MiB one, bytes included, under a third of
/// this. At the limit, parsing takes on the order of a second.
pub const WORK_BUDGET: u64 = 1 << 27;

/// A page, parsed as far as its budget allowed.
pub(crate) struct Parsed {
    pub dom: RcDom,
    /// Where parsing stopped, in bytes of the input, if it stopped early.
    pub cut_at: Option<usize>,
}

/// Parse `html`, spending at most `budget` units on it.
pub(crate) fn parse(html: &str, budget: u64) -> Parsed {
    let meter = Rc::new(Meter::new(budget));
    let sink = Metered {
        inner: RcDom::default(),
        meter: meter.clone(),
    };
    // The same options `html5ever::parse_document` uses by default.
    let builder = TreeBuilder::new(sink, TreeBuilderOpts::default());
    let gate = Gate {
        inner: builder,
        meter: meter.clone(),
        last_tag: RefCell::new(None),
        #[cfg(test)]
        seen: RefCell::new(Vec::new()),
    };
    let tokenizer = Tokenizer::new(gate, TokenizerOpts::default());
    let queue = BufferQueue::default();
    let mut ahead = Lookahead::new(html);
    // Everything before `parsed` has been parsed in full; the tokenizer has
    // been given everything before `given`.
    let mut parsed = 0;
    let mut given = 0;
    let mut cut_at = None;

    loop {
        // Only a tag changes this, and the tokenizer has seen every tag
        // before this point.
        ahead.foreign = tokenizer
            .sink
            .adjusted_current_node_present_but_not_in_html_namespace();
        // Every byte is read by the lookahead and then by the tokenizer, and
        // each costs a unit: so the lookahead reads no further than the
        // budget has units left, whatever the input holds, tag or no tag.
        let from = ahead.position;
        let limit = from.saturating_add(usize::try_from(meter.remaining()).unwrap_or(usize::MAX));
        let scan = ahead.next_tag(limit);
        meter.charge((ahead.position - from) as u64);
        let (next, start, end, attributes) = match scan {
            Scan::Tag(tag) => (Some(tag), tag.start, tag.end, tag.attributes),
            Scan::End => (
                None,
                ahead.open_tag_start.unwrap_or(html.len()),
                html.len(),
                ahead.attributes,
            ),
            Scan::Limit => {
                // As far as the budget reached, and not into a tag that
                // began before it.
                let stop = ahead.open_tag_start.unwrap_or(ahead.position);
                feed(&tokenizer, &queue, &html[given..stop]);
                given = stop;
                cut_at = Some(if meter.spent() { parsed } else { stop });
                break;
            }
        };
        // What comes before the tag: text, comments, a doctype.
        feed(&tokenizer, &queue, &html[given..start]);
        given = start;
        if meter.spent() {
            cut_at = Some(parsed);
            break;
        }
        parsed = start;
        // The tag, if its attributes are affordable; the tokenizer pays for
        // them as it reads them, so it is not given one that is not.
        if !meter.afford(attribute_cost(attributes)) {
            cut_at = Some(parsed);
            break;
        }
        feed(&tokenizer, &queue, &html[start..end]);
        given = end;
        if meter.spent() {
            cut_at = Some(parsed);
            break;
        }
        parsed = end;
        if next.is_none() {
            break;
        }
        if let Some(emitted) = tokenizer.sink.last_tag.borrow_mut().take() {
            ahead.follow(&emitted);
        }
    }
    tokenizer.end();
    debug_assert!(parsed <= given && given <= html.len());

    #[cfg(test)]
    {
        test_record::set(test_record::Record {
            predicted: std::mem::take(&mut ahead.seen),
            emitted: tokenizer.sink.seen.take(),
            fed: given,
            scanned: ahead.position,
            charged: meter.charged.get(),
        });
    }
    Parsed {
        dom: tokenizer.sink.inner.sink.inner,
        cut_at,
    }
}

fn feed(tokenizer: &Tokenizer<Gate>, queue: &BufferQueue, input: &str) {
    if input.is_empty() {
        return;
    }
    queue.push_back(StrTendril::from_slice(input));
    // A pause for a script or an encoding changes nothing here: there is
    // nothing to run, and the input is already text.
    while !matches!(tokenizer.feed(queue), TokenizerResult::Done) {}
}

/// What the tokenizer's duplicate check costs for a tag with `attributes`
/// attributes: each is compared with those before it.
fn attribute_cost(attributes: u64) -> u64 {
    attributes.saturating_mul(attributes.saturating_sub(1)) / 2 + attributes
}

/// The work done so far, against the budget.
struct Meter {
    budget: u64,
    charged: Cell<u64>,
    refused: Cell<bool>,
}

impl Meter {
    fn new(budget: u64) -> Self {
        Meter {
            budget,
            charged: Cell::new(0),
            refused: Cell::new(false),
        }
    }

    /// Work about to be done, if the budget allows it: charged and true if
    /// it does; if not, nothing is charged, the work is not to be done, and
    /// the budget is spent from here on.
    fn afford(&self, units: u64) -> bool {
        let charged = self.charged.get().saturating_add(units);
        if charged > self.budget || self.refused.get() {
            self.refused.set(true);
            return false;
        }
        self.charged.set(charged);
        true
    }

    /// Work that is being done whatever the budget says: the tree has to be
    /// kept whole for the tree builder to finish.
    fn charge(&self, units: u64) {
        let charged = self.charged.get().saturating_add(units);
        self.charged.set(charged);
        #[cfg(test)]
        test_record::tripwire(charged, self.budget);
    }

    fn spent(&self) -> bool {
        self.refused.get() || self.charged.get() > self.budget
    }

    /// Units left before the budget is spent.
    fn remaining(&self) -> u64 {
        if self.refused.get() {
            return 0;
        }
        self.budget.saturating_sub(self.charged.get())
    }
}

// ------------------------------------------------------------------- gate

/// What the tree builder did with a tag: which state it left the tokenizer
/// in, which is where [`Lookahead`] has to carry on from.
struct Emitted {
    kind: TagKind,
    name: LocalName,
    next: Next,
}

enum Next {
    Data,
    Raw(RawKind),
    Plaintext,
}

/// Between the tokenizer and the tree builder: charges each token before the
/// tree builder is given it, and once the budget is spent, gives it nothing
/// more but the end of the input.
struct Gate {
    inner: TreeBuilder<Handle, Metered>,
    meter: Rc<Meter>,
    last_tag: RefCell<Option<Emitted>>,
    #[cfg(test)]
    seen: RefCell<Vec<test_record::Tag>>,
}

impl Gate {
    /// How many elements the tree builder is holding: its per-token scans
    /// walk these.
    fn held(&self) -> u64 {
        struct Count(Cell<u64>);
        impl Tracer for Count {
            type Handle = Handle;
            fn trace_handle(&self, _: &Handle) {
                self.0.set(self.0.get() + 1);
            }
        }
        let count = Count(Cell::new(0));
        self.inner.trace_handles(&count);
        count.0.get()
    }
}

impl TokenSink for Gate {
    type Handle = Handle;

    fn process_token(&self, token: Token, line_number: u64) -> TokenSinkResult<Handle> {
        let end = matches!(token, Token::EOFToken);
        if self.meter.spent() && !end {
            return TokenSinkResult::Continue;
        }
        let attributes = match &token {
            Token::TagToken(tag) => tag.attrs.len() as u64,
            _ => 0,
        };
        let cost = 1 + self.held() + attributes;
        if end {
            // The end is always given, to close what is open. It walks the
            // stack once.
            self.meter.charge(cost);
        } else if !self.meter.afford(cost) {
            return TokenSinkResult::Continue;
        }
        let tag = match &token {
            Token::TagToken(tag) => Some((tag.kind, tag.name.clone())),
            _ => None,
        };
        #[cfg(test)]
        if let Token::TagToken(tag) = &token {
            let mut names: Vec<String> = Vec::new();
            for attribute in &tag.attrs {
                names.push(attribute.name.local.to_string());
            }
            self.seen.borrow_mut().push(test_record::Tag {
                end: tag.kind == TagKind::EndTag,
                name: tag.name.to_string(),
                attributes: names,
            });
        }
        let result = self.inner.process_token(token, line_number);
        if let Some((kind, name)) = tag {
            let next = match &result {
                TokenSinkResult::RawData(kind) => Next::Raw(*kind),
                TokenSinkResult::Plaintext => Next::Plaintext,
                _ => Next::Data,
            };
            *self.last_tag.borrow_mut() = Some(Emitted { kind, name, next });
        }
        result
    }

    fn end(&self) {
        self.inner.end()
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.inner
            .adjusted_current_node_present_but_not_in_html_namespace()
    }
}

// ------------------------------------------------------------------ metered

/// The tree, charging each change for what it walks.
struct Metered {
    inner: RcDom,
    meter: Rc<Meter>,
}

/// How many children `node`'s parent has: what finding `node` among them
/// walks.
fn siblings(node: &Handle) -> u64 {
    let parent = node.parent.take();
    let count = parent
        .as_ref()
        .and_then(|weak| weak.upgrade())
        .map_or(0, |parent| parent.children.borrow().len() as u64);
    node.parent.set(parent);
    count
}

fn moved(child: &NodeOrText<Handle>) -> u64 {
    match child {
        NodeOrText::AppendNode(node) => siblings(node),
        NodeOrText::AppendText(_) => 0,
    }
}

/// How many nodes are in `node`'s subtree, and above it.
fn extent(node: &Handle) -> u64 {
    let mut count = 0u64;
    let mut stack = vec![node.clone()];
    while let Some(next) = stack.pop() {
        count += 1;
        stack.extend(next.children.borrow().iter().cloned());
    }
    let mut above = node.parent.take();
    node.parent.set(above.clone());
    while let Some(parent) = above.and_then(|weak| weak.upgrade()) {
        count += 1;
        above = parent.parent.take();
        parent.parent.set(above.clone());
    }
    count
}

impl TreeSink for Metered {
    type Handle = Handle;
    type Output = RcDom;
    type ElemName<'a> = <RcDom as TreeSink>::ElemName<'a>;

    fn finish(self) -> RcDom {
        self.inner
    }

    // Not kept: nothing reads them, and a page of errors — a NUL in every
    // byte — would otherwise be a list as long as the page.
    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        self.inner.get_document()
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> Self::ElemName<'a> {
        self.inner.elem_name(target)
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        self.meter.charge(1 + attrs.len() as u64);
        self.inner.create_element(name, attrs, flags)
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        self.meter.charge(1);
        self.inner.create_comment(text)
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Handle {
        self.meter.charge(1);
        self.inner.create_pi(target, data)
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        self.meter.charge(1);
        self.inner.append(parent, child)
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        self.meter.charge(1 + siblings(element) + moved(&child));
        self.inner
            .append_based_on_parent_node(element, prev_element, child)
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        self.meter.charge(1);
        self.inner
            .append_doctype_to_document(name, public_id, system_id)
    }

    fn mark_script_already_started(&self, node: &Handle) {
        self.inner.mark_script_already_started(node)
    }

    fn pop(&self, node: &Handle) {
        self.inner.pop(node)
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        self.inner.get_template_contents(target)
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        self.inner.same_node(x, y)
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.inner.set_quirks_mode(mode)
    }

    fn append_before_sibling(&self, sibling: &Handle, new_node: NodeOrText<Handle>) {
        self.meter.charge(1 + siblings(sibling) + moved(&new_node));
        self.inner.append_before_sibling(sibling, new_node)
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        let existing = match &target.data {
            NodeData::Element { attrs, .. } => attrs.borrow().len() as u64,
            _ => 0,
        };
        // Weighted: each call hashes every attribute already there.
        self.meter.charge(1 + 4 * existing + attrs.len() as u64);
        self.inner.add_attrs_if_missing(target, attrs)
    }

    fn associate_with_form(
        &self,
        target: &Handle,
        form: &Handle,
        nodes: (&Handle, Option<&Handle>),
    ) {
        self.inner.associate_with_form(target, form, nodes)
    }

    fn remove_from_parent(&self, target: &Handle) {
        self.meter.charge(1 + siblings(target));
        self.inner.remove_from_parent(target)
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        self.meter.charge(1 + node.children.borrow().len() as u64);
        self.inner.reparent_children(node, new_parent)
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Handle) -> bool {
        self.inner
            .is_mathml_annotation_xml_integration_point(handle)
    }

    fn set_current_line(&self, line_number: u64) {
        self.inner.set_current_line(line_number)
    }

    fn allow_declarative_shadow_roots(&self, intended_parent: &Handle) -> bool {
        self.inner.allow_declarative_shadow_roots(intended_parent)
    }

    fn attach_declarative_shadow(
        &self,
        location: &Handle,
        template: &Handle,
        attrs: &[Attribute],
    ) -> bool {
        self.inner
            .attach_declarative_shadow(location, template, attrs)
    }

    fn maybe_clone_an_option_into_selectedcontent(&self, option: &Handle) {
        // Finds the select above the option and copies the option's subtree.
        self.meter.charge(1 + extent(option));
        self.inner
            .maybe_clone_an_option_into_selectedcontent(option)
    }
}

// ---------------------------------------------------------------- lookahead

/// Where the tokenizer is, as far as where its tags begin and end.
///
/// These are html5ever's tokenizer states (0.39), less what only affects the
/// text a token carries: character references, NUL replacement and the
/// like. They are followed exactly, not approximated, because an
/// approximation that thinks a real tag is inside a comment is a tag whose
/// attributes nobody counted. `parse` puts [`Lookahead`] back in step with
/// the tokenizer after every tag the tree builder sees, from what the tree
/// builder told the tokenizer to do next, and a test runs both over random
/// input to check they never disagree about a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    TagOpen,
    EndTagOpen,
    TagName,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    DoubleQuoted,
    SingleQuoted,
    Unquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentLessThanSign,
    CommentLessThanSignBang,
    CommentLessThanSignBangDash,
    CommentLessThanSignBangDashDash,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    BogusComment,
    Doctype,
    CdataSection,
    CdataSectionBracket,
    CdataSectionEnd,
    Raw(RawKind),
    RawLessThanSign(RawKind),
    RawEndTagOpen(RawKind),
    RawEndTagName(RawKind),
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataDoubleEscapeStart,
    ScriptDataEscapedDash(ScriptEscapeKind),
    ScriptDataEscapedDashDash(ScriptEscapeKind),
    ScriptDataDoubleEscapeEnd,
    Plaintext,
}

/// How far [`Lookahead::next_tag`] got.
#[derive(Debug, Clone, Copy)]
enum Scan {
    /// To the end of a tag.
    Tag(Found),
    /// To the end of the input, with no tag finished on the way.
    End,
    /// To the byte limit it was given, with no tag finished on the way.
    Limit,
}

/// A tag, found ahead of the tokenizer.
#[derive(Debug, Clone, Copy)]
struct Found {
    /// The `<` it begins with.
    start: usize,
    /// Just past the `>` it ends with.
    end: usize,
    /// How many attributes the tokenizer will create for it, duplicates
    /// included: each is checked against the others.
    attributes: u64,
}

struct Lookahead<'a> {
    input: &'a str,
    position: usize,
    state: State,
    /// The tag being read, and where it began.
    open_tag_start: Option<usize>,
    kind: TagKind,
    attributes: u64,
    /// The tag name being read, lowercased: needed to know whether an end
    /// tag closes raw text.
    name: String,
    /// The name of the last start tag, which is the end tag that closes raw
    /// text.
    last_start: String,
    /// The tokenizer's temporary buffer, in the script data states.
    temp: String,
    /// Whether the tree builder's adjusted current node is foreign content,
    /// where `<![CDATA[` opens a CDATA section rather than a bogus comment.
    foreign: bool,
    #[cfg(test)]
    seen: Vec<test_record::Tag>,
    #[cfg(test)]
    attribute_names: Vec<String>,
}

fn whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | ' ' | '\r')
}

impl<'a> Lookahead<'a> {
    fn new(input: &'a str) -> Self {
        Lookahead {
            input,
            position: 0,
            state: State::Data,
            open_tag_start: None,
            kind: TagKind::StartTag,
            attributes: 0,
            name: String::new(),
            last_start: String::new(),
            temp: String::new(),
            foreign: false,
            #[cfg(test)]
            seen: Vec::new(),
            #[cfg(test)]
            attribute_names: Vec::new(),
        }
    }

    /// Carry on from where the tree builder left the tokenizer after the tag
    /// it was just given.
    fn follow(&mut self, emitted: &Emitted) {
        if emitted.kind == TagKind::StartTag {
            self.last_start = emitted.name.to_string();
        }
        self.state = match emitted.next {
            Next::Data => State::Data,
            Next::Raw(kind) => State::Raw(kind),
            Next::Plaintext => State::Plaintext,
        };
    }

    fn begin_tag(&mut self, kind: TagKind, start: usize, first: char) {
        self.open_tag_start = Some(start);
        self.kind = kind;
        self.attributes = 0;
        self.name.clear();
        self.name.push(first.to_ascii_lowercase());
        #[cfg(test)]
        self.attribute_names.clear();
    }

    fn begin_attribute(&mut self, _first: char) {
        self.attributes += 1;
        #[cfg(test)]
        self.attribute_names
            .push(test_record::name_char(_first).to_string());
    }

    fn push_attribute_name(&mut self, _c: char) {
        #[cfg(test)]
        if let Some(name) = self.attribute_names.last_mut() {
            name.push(test_record::name_char(_c));
        }
    }

    fn emit(&mut self, end: usize) -> Found {
        let found = Found {
            start: self.open_tag_start.take().unwrap_or(end),
            end,
            attributes: self.attributes,
        };
        if self.kind == TagKind::StartTag {
            self.last_start = self.name.clone();
        }
        #[cfg(test)]
        {
            let mut unique: Vec<String> = Vec::new();
            for name in self.attribute_names.drain(..) {
                if !unique.contains(&name) {
                    unique.push(name);
                }
            }
            self.seen.push(test_record::Tag {
                end: self.kind == TagKind::EndTag,
                name: self.name.chars().map(test_record::name_char).collect(),
                attributes: unique,
            });
        }
        self.attributes = 0;
        self.state = State::Data;
        found
    }

    fn appropriate(&self) -> bool {
        self.kind == TagKind::EndTag && self.name == self.last_start
    }

    /// Read on to the end of the next tag, reading no character that starts
    /// at or past `limit`. At the end of the input, [`Self::open_tag_start`]
    /// and [`Self::attributes`] describe a tag the input ended inside, if it
    /// did.
    fn next_tag(&mut self, limit: usize) -> Scan {
        use RawKind::{Rawtext, Rcdata, ScriptData, ScriptDataEscaped};
        use ScriptEscapeKind::{DoubleEscaped, Escaped};
        use State as S;

        let input = self.input;
        // `reconsume` holds a character back to be read again in the new state.
        let mut reconsume: Option<(usize, char)> = None;
        let base = self.position;
        let mut chars = input[base..].char_indices();
        loop {
            let (at, c) = match reconsume.take() {
                Some(held) => held,
                None => match chars.next() {
                    Some((offset, _)) if base + offset >= limit => {
                        self.position = base + offset;
                        return Scan::Limit;
                    }
                    Some((offset, c)) => (base + offset, c),
                    None => {
                        self.position = input.len();
                        return Scan::End;
                    }
                },
            };
            let after = at + c.len_utf8();
            self.position = after;
            match self.state {
                S::Data => {
                    if c == '<' {
                        self.state = S::TagOpen;
                        self.open_tag_start = Some(at);
                    }
                }
                S::Plaintext => {}
                S::TagOpen => match c {
                    '!' => self.state = S::MarkupDeclarationOpen,
                    '/' => self.state = S::EndTagOpen,
                    '?' => self.state = S::BogusComment,
                    c if c.is_ascii_alphabetic() => {
                        let start = at - 1;
                        self.begin_tag(TagKind::StartTag, start, c);
                        self.state = S::TagName;
                    }
                    _ => {
                        self.open_tag_start = None;
                        self.state = S::Data;
                        reconsume = Some((at, c));
                    }
                },
                S::EndTagOpen => match c {
                    '>' => {
                        self.open_tag_start = None;
                        self.state = S::Data;
                    }
                    c if c.is_ascii_alphabetic() => {
                        let start = at - 2;
                        self.begin_tag(TagKind::EndTag, start, c);
                        self.state = S::TagName;
                    }
                    _ => {
                        self.open_tag_start = None;
                        self.state = S::BogusComment;
                        reconsume = Some((at, c));
                    }
                },
                S::TagName => match c {
                    c if whitespace(c) => self.state = S::BeforeAttributeName,
                    '/' => self.state = S::SelfClosingStartTag,
                    '>' => return Scan::Tag(self.emit(after)),
                    c => self.name.push(c.to_ascii_lowercase()),
                },
                S::BeforeAttributeName => match c {
                    c if whitespace(c) => {}
                    '/' => self.state = S::SelfClosingStartTag,
                    '>' => return Scan::Tag(self.emit(after)),
                    c => {
                        self.begin_attribute(c);
                        self.state = S::AttributeName;
                    }
                },
                S::AttributeName => match c {
                    c if whitespace(c) => self.state = S::AfterAttributeName,
                    '/' => self.state = S::SelfClosingStartTag,
                    '=' => self.state = S::BeforeAttributeValue,
                    '>' => return Scan::Tag(self.emit(after)),
                    c => self.push_attribute_name(c),
                },
                S::AfterAttributeName => match c {
                    c if whitespace(c) => {}
                    '/' => self.state = S::SelfClosingStartTag,
                    '=' => self.state = S::BeforeAttributeValue,
                    '>' => return Scan::Tag(self.emit(after)),
                    c => {
                        self.begin_attribute(c);
                        self.state = S::AttributeName;
                    }
                },
                S::BeforeAttributeValue => match c {
                    c if whitespace(c) => {}
                    '"' => self.state = S::DoubleQuoted,
                    '\'' => self.state = S::SingleQuoted,
                    '>' => return Scan::Tag(self.emit(after)),
                    c => {
                        self.state = S::Unquoted;
                        reconsume = Some((at, c));
                    }
                },
                S::DoubleQuoted => {
                    if c == '"' {
                        self.state = S::AfterAttributeValueQuoted;
                    }
                }
                S::SingleQuoted => {
                    if c == '\'' {
                        self.state = S::AfterAttributeValueQuoted;
                    }
                }
                S::Unquoted => match c {
                    '\t' | '\n' | '\x0C' | ' ' | '\r' => self.state = S::BeforeAttributeName,
                    '>' => return Scan::Tag(self.emit(after)),
                    _ => {}
                },
                S::AfterAttributeValueQuoted => match c {
                    c if whitespace(c) => self.state = S::BeforeAttributeName,
                    '/' => self.state = S::SelfClosingStartTag,
                    '>' => return Scan::Tag(self.emit(after)),
                    c => {
                        self.state = S::BeforeAttributeName;
                        reconsume = Some((at, c));
                    }
                },
                S::SelfClosingStartTag => match c {
                    '>' => return Scan::Tag(self.emit(after)),
                    c => {
                        self.state = S::BeforeAttributeName;
                        reconsume = Some((at, c));
                    }
                },
                S::MarkupDeclarationOpen => {
                    // Decided on what follows `<!`, all at once, as the
                    // tokenizer does.
                    let rest = &input[at..];
                    self.open_tag_start = None;
                    if rest.starts_with("--") {
                        self.state = S::CommentStart;
                        chars.next();
                    } else if rest.len() >= 7
                        && rest.as_bytes()[..7].eq_ignore_ascii_case(b"doctype")
                    {
                        self.state = S::Doctype;
                        for _ in 0..6 {
                            chars.next();
                        }
                    } else if rest.starts_with("[CDATA[") && self.foreign {
                        self.state = S::CdataSection;
                        for _ in 0..6 {
                            chars.next();
                        }
                    } else {
                        self.state = S::BogusComment;
                        reconsume = Some((at, c));
                    }
                }
                S::CommentStart => match c {
                    '-' => self.state = S::CommentStartDash,
                    '>' => self.state = S::Data,
                    _ => self.state = S::Comment,
                },
                S::CommentStartDash => match c {
                    '-' => self.state = S::CommentEnd,
                    '>' => self.state = S::Data,
                    _ => self.state = S::Comment,
                },
                S::Comment => match c {
                    '<' => self.state = S::CommentLessThanSign,
                    '-' => self.state = S::CommentEndDash,
                    _ => {}
                },
                S::CommentLessThanSign => match c {
                    '!' => self.state = S::CommentLessThanSignBang,
                    '<' => {}
                    c => {
                        self.state = S::Comment;
                        reconsume = Some((at, c));
                    }
                },
                S::CommentLessThanSignBang => match c {
                    '-' => self.state = S::CommentLessThanSignBangDash,
                    c => {
                        self.state = S::Comment;
                        reconsume = Some((at, c));
                    }
                },
                S::CommentLessThanSignBangDash => match c {
                    '-' => self.state = S::CommentLessThanSignBangDashDash,
                    c => {
                        self.state = S::CommentEndDash;
                        reconsume = Some((at, c));
                    }
                },
                S::CommentLessThanSignBangDashDash => {
                    self.state = S::CommentEnd;
                    reconsume = Some((at, c));
                }
                S::CommentEndDash => match c {
                    '-' => self.state = S::CommentEnd,
                    _ => self.state = S::Comment,
                },
                S::CommentEnd => match c {
                    '>' => self.state = S::Data,
                    '!' => self.state = S::CommentEndBang,
                    '-' => {}
                    c => {
                        self.state = S::Comment;
                        reconsume = Some((at, c));
                    }
                },
                S::CommentEndBang => match c {
                    '-' => self.state = S::CommentEndDash,
                    '>' => self.state = S::Data,
                    _ => self.state = S::Comment,
                },
                S::BogusComment | S::Doctype => {
                    if c == '>' {
                        self.state = S::Data;
                    }
                }
                S::CdataSection => {
                    if c == ']' {
                        self.state = S::CdataSectionBracket;
                    }
                }
                S::CdataSectionBracket => match c {
                    ']' => self.state = S::CdataSectionEnd,
                    c => {
                        self.state = S::CdataSection;
                        reconsume = Some((at, c));
                    }
                },
                S::CdataSectionEnd => match c {
                    ']' => {}
                    '>' => self.state = S::Data,
                    c => {
                        self.state = S::CdataSection;
                        reconsume = Some((at, c));
                    }
                },
                S::Raw(kind) => match (kind, c) {
                    (Rcdata | Rawtext | ScriptData, '<') => {
                        self.open_tag_start = Some(at);
                        self.state = S::RawLessThanSign(kind);
                    }
                    (ScriptDataEscaped(escape), '-') => {
                        self.state = S::ScriptDataEscapedDash(escape);
                    }
                    (ScriptDataEscaped(_), '<') => {
                        self.open_tag_start = Some(at);
                        self.state = S::RawLessThanSign(kind);
                    }
                    _ => {}
                },
                S::RawLessThanSign(ScriptDataEscaped(Escaped)) => match c {
                    '/' => self.state = S::RawEndTagOpen(ScriptDataEscaped(Escaped)),
                    c if c.is_ascii_alphabetic() => {
                        self.temp.clear();
                        self.temp.push(c.to_ascii_lowercase());
                        self.state = S::ScriptDataDoubleEscapeStart;
                    }
                    c => {
                        self.state = S::Raw(ScriptDataEscaped(Escaped));
                        reconsume = Some((at, c));
                    }
                },
                S::RawLessThanSign(ScriptDataEscaped(DoubleEscaped)) => match c {
                    '/' => {
                        self.temp.clear();
                        self.state = S::ScriptDataDoubleEscapeEnd;
                    }
                    c => {
                        self.state = S::Raw(ScriptDataEscaped(DoubleEscaped));
                        reconsume = Some((at, c));
                    }
                },
                S::RawLessThanSign(kind) => match c {
                    '/' => self.state = S::RawEndTagOpen(kind),
                    '!' if kind == ScriptData => self.state = S::ScriptDataEscapeStart,
                    c => {
                        self.state = S::Raw(kind);
                        reconsume = Some((at, c));
                    }
                },
                S::RawEndTagOpen(kind) => match c {
                    c if c.is_ascii_alphabetic() => {
                        let start = self.open_tag_start.unwrap_or(at);
                        self.begin_tag(TagKind::EndTag, start, c);
                        self.state = S::RawEndTagName(kind);
                    }
                    c => {
                        self.state = S::Raw(kind);
                        reconsume = Some((at, c));
                    }
                },
                S::RawEndTagName(kind) => {
                    if self.appropriate() {
                        match c {
                            c if whitespace(c) => {
                                self.state = S::BeforeAttributeName;
                                continue;
                            }
                            '/' => {
                                self.state = S::SelfClosingStartTag;
                                continue;
                            }
                            '>' => return Scan::Tag(self.emit(after)),
                            _ => {}
                        }
                    }
                    if c.is_ascii_alphabetic() {
                        self.name.push(c.to_ascii_lowercase());
                    } else {
                        self.open_tag_start = None;
                        self.state = S::Raw(kind);
                        reconsume = Some((at, c));
                    }
                }
                S::ScriptDataEscapeStart => match c {
                    '-' => self.state = S::ScriptDataEscapeStartDash,
                    c => {
                        self.state = S::Raw(ScriptData);
                        reconsume = Some((at, c));
                    }
                },
                S::ScriptDataEscapeStartDash => match c {
                    '-' => self.state = S::ScriptDataEscapedDashDash(Escaped),
                    c => {
                        self.state = S::Raw(ScriptData);
                        reconsume = Some((at, c));
                    }
                },
                S::ScriptDataEscapedDash(escape) => match c {
                    '-' => self.state = S::ScriptDataEscapedDashDash(escape),
                    '<' => {
                        self.open_tag_start = Some(at);
                        self.state = S::RawLessThanSign(ScriptDataEscaped(escape));
                    }
                    _ => self.state = S::Raw(ScriptDataEscaped(escape)),
                },
                S::ScriptDataEscapedDashDash(escape) => match c {
                    '-' => {}
                    '<' => {
                        self.open_tag_start = Some(at);
                        self.state = S::RawLessThanSign(ScriptDataEscaped(escape));
                    }
                    '>' => self.state = S::Raw(ScriptData),
                    _ => self.state = S::Raw(ScriptDataEscaped(escape)),
                },
                S::ScriptDataDoubleEscapeStart => match c {
                    c if whitespace(c) || c == '/' || c == '>' => {
                        let escape = if self.temp == "script" {
                            DoubleEscaped
                        } else {
                            Escaped
                        };
                        self.state = S::Raw(ScriptDataEscaped(escape));
                    }
                    c if c.is_ascii_alphabetic() => self.temp.push(c.to_ascii_lowercase()),
                    c => {
                        self.state = S::Raw(ScriptDataEscaped(Escaped));
                        reconsume = Some((at, c));
                    }
                },
                S::ScriptDataDoubleEscapeEnd => match c {
                    c if whitespace(c) || c == '/' || c == '>' => {
                        let escape = if self.temp == "script" {
                            Escaped
                        } else {
                            DoubleEscaped
                        };
                        self.state = S::Raw(ScriptDataEscaped(escape));
                    }
                    c if c.is_ascii_alphabetic() => self.temp.push(c.to_ascii_lowercase()),
                    c => {
                        self.state = S::Raw(ScriptDataEscaped(DoubleEscaped));
                        reconsume = Some((at, c));
                    }
                },
            }
            // Only the states that may still become a tag keep its start.
            if !reading_a_tag(self.state) {
                self.open_tag_start = None;
            }
        }
    }
}

/// Whether the tokenizer, in `state`, is reading what is or may yet become
/// a tag.
fn reading_a_tag(state: State) -> bool {
    use State as S;
    matches!(
        state,
        S::TagOpen
            | S::EndTagOpen
            | S::TagName
            | S::BeforeAttributeName
            | S::AttributeName
            | S::AfterAttributeName
            | S::BeforeAttributeValue
            | S::DoubleQuoted
            | S::SingleQuoted
            | S::Unquoted
            | S::AfterAttributeValueQuoted
            | S::SelfClosingStartTag
            | S::RawLessThanSign(_)
            | S::RawEndTagOpen(_)
            | S::RawEndTagName(_)
    )
}

#[cfg(test)]
pub(crate) mod test_record {
    //! What a parse did, kept for the tests that check the budget and the
    //! lookahead against html5ever itself.

    use std::cell::{Cell, RefCell};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Tag {
        pub end: bool,
        pub name: String,
        pub attributes: Vec<String>,
    }

    pub struct Record {
        /// The tags [`super::Lookahead`] found, in order.
        pub predicted: Vec<Tag>,
        /// The tags the tokenizer gave the tree builder, in order.
        pub emitted: Vec<Tag>,
        /// How much of the input the tokenizer was given.
        pub fed: usize,
        /// How much of the input the lookahead read.
        pub scanned: usize,
        /// The work charged.
        pub charged: u64,
    }

    thread_local! {
        static LAST: RefCell<Option<Record>> = const { RefCell::new(None) };
        static TRIPWIRE: Cell<bool> = const { Cell::new(false) };
    }

    pub fn set(record: Record) {
        LAST.with(|last| *last.borrow_mut() = Some(record));
    }

    pub fn take() -> Record {
        LAST.with(|last| last.borrow_mut().take())
            .expect("nothing was parsed")
    }

    /// Fail, rather than run for minutes, if a parse is let run on well past
    /// its budget: what a missing bound looks like.
    pub fn arm_tripwire() {
        TRIPWIRE.with(|armed| armed.set(true));
    }

    pub fn tripwire(charged: u64, budget: u64) {
        if TRIPWIRE.with(Cell::get) && charged > budget.saturating_mul(8).max(1 << 16) {
            panic!("the parse went on to {charged} units against a budget of {budget}");
        }
    }

    /// A name character as html5ever stores it.
    pub fn name_char(c: char) -> char {
        if c == '\0' {
            '\u{fffd}'
        } else {
            c.to_ascii_lowercase()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    /// Parse with a budget nothing ordinary reaches, and return what the
    /// lookahead predicted and what the tokenizer did.
    fn both(html: &str) -> test_record::Record {
        let parsed = parse(html, u64::MAX / 2);
        assert!(parsed.cut_at.is_none());
        test_record::take()
    }

    fn agree(html: &str) {
        let record = both(html);
        assert_eq!(record.fed, html.len(), "not all of {html:?} was parsed");
        assert_eq!(
            record.predicted, record.emitted,
            "the lookahead and the tokenizer disagree on {html:?}"
        );
    }

    /// Everything that changes where a tag begins or ends, one at a time.
    #[test]
    fn the_lookahead_finds_every_tag_the_tokenizer_does() {
        for html in [
            "<p>plain <b class=x id='y' data-z=\"w\">text</b></p>",
            "<a b c d e f>dup <i a a a b a>attrs</i>",
            "<div a=\"x > y\" b='>' c=d>quoted</div>",
            "<br/><img src=x / ><x/y z>",
            "<!-- <a href=x> --><b>after</b>",
            "<!--> <i> <!---> <u> <!-- a --!> <s> <!-- <!-- x --> <q>",
            "<!-- - -- --- ! --! <!- <!-- <!--> --> <em>",
            "<!doctype html><!DOCTYPE x 'a > b'><p>",
            "<?php echo '<b>'; ?><i>",
            "</ x><//><i></>text</3><em>",
            "<3 a<b a < b <a",
            "<![CDATA[ <b> ]]><i>",
            "<svg><![CDATA[ <b> ]]></svg><i>",
            "<math><![CDATA[x]]]]>y]]><mi>",
            "<script>if (a<b && c>d) '<i>'</script><b>",
            "<script>x</scriptx></script ><i></SCRIPT><u>",
            "<script><!-- <script> </script> --> </script><b>",
            "<script><!--<script>x</script>--></script><b>",
            "<script><!-- </script><i>",
            "<script><!--- -><script>-->--></script><em>",
            "<style>p > b { }</style><b>",
            "<title>a <b> c</title><i>",
            "<textarea></textarea x=y></textarea><u>",
            "<xmp><b></xmp><i>",
            "<noscript><b></noscript><i>",
            "<iframe><b></iframe><i>",
            "<plaintext><b></plaintext><i>",
            "<svg><style><b></style></svg><i>",
            "<svg><script><b></script></svg><i>",
            "<table><tr><td>x<script>a<b</script>",
            "<p a=\"unterminated",
            "<p a=x\r\nb=y\x0Cc=z\td>",
            "<p\x0Ca\x0C=\x0C'1'\x0Cb>",
            "<p\0a \0b=\0>x</p\0>",
            "<P CLASS=A ÉTÉ=Ü>ünïcödé</P>",
            "<a =x ==y \"q' 'r\">",
            "<!-",
            "<!--",
            "<",
            "</",
            "<script>",
            "",
        ] {
            agree(html);
        }
    }

    /// Random pages from pieces that each matter to the tokenizer.
    #[test]
    fn the_lookahead_and_the_tokenizer_agree_on_random_pages() {
        const PIECES: &[&str] = &[
            "<",
            ">",
            "</",
            "/",
            "!",
            "-",
            "--",
            "?",
            "\"",
            "'",
            "=",
            " ",
            "\t",
            "\r\n",
            "\0",
            "\x0C",
            "\r",
            "a",
            "B",
            "é",
            "&amp;",
            "&",
            "]]>",
            "]",
            "[CDATA[",
            "<![CDATA[",
            "<!--",
            "-->",
            "--!>",
            "<!",
            "DOCTYPE",
            "<!doctype",
            "div",
            "p",
            "script",
            "SCRIPT",
            "style",
            "textarea",
            "title",
            "xmp",
            "iframe",
            "noembed",
            "noframes",
            "noscript",
            "plaintext",
            "svg",
            "math",
            "mi",
            "foreignObject",
            "desc",
            "table",
            "tr",
            "td",
            "template",
            "select",
            "option",
            "html",
            "body",
            "head",
            "<script>",
            "</script>",
            "<svg>",
            "</svg>",
            "<math>",
            "<table>",
            "<p ",
            " x=1",
            " y='2'",
            " z=\"3\"",
        ];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..4_000 {
            let length = 1 + next() % 60;
            let html: String = (0..length)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            agree(&html);
        }
    }

    /// Arm the tripwire, parse `html` through the public path with a small
    /// budget, and return the document and what the parse did. With the
    /// bound in place the parse stops near the budget; without it, the
    /// tripwire ends the test long before the page would.
    fn hostile(html: &str) -> (Document, test_record::Record) {
        test_record::arm_tripwire();
        let document = Document::parse_within(html, Some("https://hostile.test/"), 1 << 20);
        (document, test_record::take())
    }

    #[test]
    fn a_page_nested_two_hundred_thousand_deep_is_cut_short() {
        let html = format!("<title>deep</title>{}bottom", "<div>".repeat(200_000));
        let (document, record) = hostile(&html);
        let cut = document.cut_short().expect("parsed to the end");
        assert!(cut.parsed < html.len() / 10, "{cut:?}");
        assert_eq!(cut.of, html.len());
        assert!(
            record.charged <= (1 << 20) + (1 << 16),
            "{}",
            record.charged
        );
        // What was parsed is there.
        assert_eq!(document.title().as_deref(), Some("deep"));
    }

    #[test]
    fn a_tag_with_twenty_thousand_attributes_never_reaches_the_tokenizer() {
        let attributes: String = (0..20_000).map(|i| format!(" a{i}")).collect();
        let prefix = "<title>before</title><p>";
        let html = format!("{prefix}<b{attributes}>after");
        let (document, record) = hostile(&html);
        let cut = document.cut_short().expect("parsed to the end");
        assert_eq!(cut.parsed, prefix.len());
        assert_eq!(record.fed, prefix.len(), "the tokenizer was given the tag");
        assert_eq!(document.title().as_deref(), Some("before"));
    }

    #[test]
    fn repeated_html_tags_merging_attributes_are_cut_short() {
        let html: String = (0..40_000)
            .map(|i| format!("<html a{i}=1 b{i}=1>"))
            .collect();
        let (document, record) = hostile(&html);
        assert!(document.cut_short().is_some());
        assert!(
            record.charged <= (1 << 20) + (1 << 16),
            "{}",
            record.charged
        );
    }

    /// Few enough tokens that only the search for the table among its
    /// siblings, once per run of text, can spend the budget; and all of it in
    /// one stretch without a tag, so only the gate can stop it.
    #[test]
    fn text_foster_parented_beside_many_siblings_is_cut_short() {
        let html = format!(
            "{}<table>{}",
            "<i></i>".repeat(4_000),
            "x<!---->".repeat(4_000)
        );
        let (document, record) = hostile(&html);
        assert!(document.cut_short().is_some());
        assert!(
            record.charged <= (1 << 20) + (1 << 16),
            "{}",
            record.charged
        );
    }

    #[test]
    fn end_tags_scanning_a_deep_stack_are_cut_short() {
        let html = format!("{}{}", "<span>".repeat(1_000), "</div>".repeat(200_000));
        let (document, _) = hostile(&html);
        assert!(document.cut_short().is_some());
    }

    /// Input that never reaches a tag, or never finishes one, is read only
    /// as far as the budget goes: by the lookahead and by the tokenizer.
    #[test]
    fn a_long_stretch_without_a_tag_is_read_only_as_far_as_the_budget_goes() {
        const BUDGET: usize = 1 << 20;
        let long = "word ".repeat(800_000);
        let prefix = "<title>t</title>";
        for (html, parsed_at_most) in [
            (long.clone(), BUDGET),
            (format!("{prefix}<p>{long}"), BUDGET),
            (format!("{prefix}<p a=\"{long}"), prefix.len()),
            (
                format!("{prefix}<p a='x' b={}", "v".repeat(4_000_000)),
                prefix.len(),
            ),
            (format!("{prefix}<p{}", "a".repeat(4_000_000)), prefix.len()),
            (format!("{prefix}<!--{long}"), BUDGET),
            (format!("{prefix}<script>{long}"), BUDGET),
            (format!("{prefix}<textarea>{long}"), BUDGET),
            // Each stretch well inside the budget; all of them together not.
            (
                format!("<br>{}", "word ".repeat(100_000)).repeat(20),
                BUDGET,
            ),
        ] {
            let (document, record) = hostile(&html);
            let cut = document
                .cut_short()
                .unwrap_or_else(|| panic!("{:?}… parsed whole", &html[..30]));
            assert!(cut.parsed <= parsed_at_most, "{:?}…: {cut:?}", &html[..30]);
            assert!(
                record.scanned <= BUDGET,
                "{:?}…: read {}",
                &html[..30],
                record.scanned
            );
            assert!(
                record.fed <= BUDGET,
                "{:?}…: fed {}",
                &html[..30],
                record.fed
            );
        }
    }

    /// An ordinary page of 16 MiB — articles, headings, links, lists,
    /// tables, attributes — is parsed whole, well inside the budget.
    #[test]
    fn an_ordinary_sixteen_mebibyte_page_is_parsed_whole() {
        let section = r#"<section class="post" id="p"><h2><a href="/a/b?c=d">A heading</a></h2>
<p class="lead">Some ordinary prose, with <em>emphasis</em>, <a href="https://example.test/x" rel="nofollow">a link</a> and <code>code</code>. It goes on for a while, as prose does, and says nothing in particular.</p>
<ul class="list"><li><a href="/1">one</a></li><li><a href="/2">two</a></li><li><a href="/3">three</a></li></ul>
<table class="data"><tr><th>k</th><th>v</th></tr><tr><td>a</td><td>1</td></tr><tr><td>b</td><td>2</td></tr></table>
<figure><img src="/img/x.png" alt="a picture" width="640" height="480"><figcaption>A caption.</figcaption></figure>
</section>
"#;
        let mut html =
            String::from("<!doctype html><html><head><title>big</title></head><body><main>");
        while html.len() < 16 * 1024 * 1024 {
            html.push_str(section);
        }
        html.push_str("</main></body></html>");
        let document = Document::parse(&html, None);
        assert_eq!(document.cut_short(), None);
        assert_eq!(document.title().as_deref(), Some("big"));
        let record = test_record::take();
        assert!(record.charged < WORK_BUDGET / 3, "{}", record.charged);
    }

    /// Large, but ordinary: long attribute values, a long comment, a long
    /// script, many elements. None of it is cut short.
    #[test]
    fn an_ordinary_large_page_is_parsed_whole() {
        let image = format!(
            "<img src=\"data:image/png;base64,{}\">",
            "iVBORw0KGgo/+=".repeat(80_000)
        );
        let props = format!(
            "<div data-props='{}'></div>",
            "{\"key\": [1, \"v\"], ".repeat(20_000)
        );
        let comment = format!("<!-- {} -->", "<b> x=1 ".repeat(50_000));
        let script = format!(
            "<script>{}</script>",
            "if (a<b) { c = \"</b>\"; }\n".repeat(20_000)
        );
        let rows = "<tr><td class=a>cell</td><td>cell</td></tr>".repeat(20_000);
        let html = format!(
            "<!doctype html><title>big</title>{image}{props}{comment}{script}<table>{rows}</table>"
        );
        let document = Document::parse(&html, None);
        assert_eq!(document.cut_short(), None);
        assert_eq!(document.title().as_deref(), Some("big"));
        let record = test_record::take();
        assert!(record.charged < WORK_BUDGET / 4, "{}", record.charged);
    }
}
