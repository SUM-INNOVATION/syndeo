//! What each accessor returns for an ordinary, well-formed page, pinned. The
//! traversals were rewritten without recursion; for a page like this nothing
//! they return may change, except the one thing that was meant to (see
//! `blocks`).

use syndeo_dom::{Document, Form, FormField, Link, Subresource, TextBlock};

const FIXTURE: &str = r#"<!doctype html><html><head><title> The  Page </title>
<script>var hidden = 1;</script><style>p{}</style>
<link rel="stylesheet" href="/a.css" integrity="sha384-AAAA">
<script src="https://cdn.test/lib.js" integrity="sha256-BBBB"></script></head>
<body><nav><a href="/home">Home</a> <a href="https://other.test/x?y=1">Other</a></nav>
<main><h1>Heading one</h1><p>First <b>bold</b> and <i>italic</i> text.<br>After a break.</p>
<ul><li>One</li><li>Two <a href="two.html">link</a></li></ul>
<table><tr><td>cell a</td><td>cell b</td></tr></table>
<div>Outer <div>inner block</div> tail</div>
<form action="/submit" method="post"><input name="q" required><input type="checkbox" name="c" value="1">
<textarea name="t"></textarea><select name="s"></select><button name="go">Go</button></form>
<img src="/i.png"><pre>  pre   text  </pre></main></body></html>"#;

fn document() -> Document {
    Document::parse(FIXTURE, Some("https://site.test/dir/page"))
}

fn block(element: &str, text: &str, heading_level: Option<u8>) -> TextBlock {
    TextBlock {
        element: element.into(),
        text: text.into(),
        heading_level,
    }
}

#[test]
fn title_and_text_are_as_before() {
    let d = document();
    assert_eq!(d.title().as_deref(), Some("The  Page"));
    assert_eq!(
        d.text(),
        "Home Other\n\nHeading one\n\nFirst bold and italic text.\n\nAfter a break.\n\nOne\n\n\
         Two link\n\ncell a\n\ncell b\n\nOuter\ninner block\ntail\n\nGo\n\npre text"
    );
}

#[test]
fn blocks_are_as_before_but_each_text_belongs_to_one_block() {
    // What changed: the table's cells used to be counted a second time for the
    // table, reached through its tbody. Each is its own block now, once.
    assert_eq!(
        document().blocks(),
        vec![
            block("nav", "Home Other", None),
            block("h1", "Heading one", Some(1)),
            block("p", "First bold and italic text.After a break.", None),
            block("li", "One", None),
            block("li", "Two link", None),
            block("td", "cell a", None),
            block("td", "cell b", None),
            block("div", "Outer tail", None),
            block("div", "inner block", None),
            block("form", "Go", None),
            block("pre", "pre text", None),
        ]
    );
}

#[test]
fn links_subresources_and_forms_are_as_before() {
    let d = document();
    let link = |url: &str, text: &str| Link {
        url: url.into(),
        text: text.into(),
        rel: None,
    };
    assert_eq!(
        d.links(),
        vec![
            link("https://site.test/home", "Home"),
            link("https://other.test/x?y=1", "Other"),
            link("https://site.test/dir/two.html", "link"),
        ]
    );
    let resource = |url: &str, kind: &str, integrity: Option<&str>| Subresource {
        url: url.into(),
        kind: kind.into(),
        integrity: integrity.map(str::to_string),
        crossorigin: None,
    };
    assert_eq!(
        d.subresources(),
        vec![
            resource("https://site.test/a.css", "stylesheet", Some("sha384-AAAA")),
            resource("https://cdn.test/lib.js", "script", Some("sha256-BBBB")),
            resource("https://site.test/i.png", "image", None),
        ]
    );
    let field = |name: &str, kind: &str, value: Option<&str>, required: bool| FormField {
        name: name.into(),
        kind: kind.into(),
        value: value.map(str::to_string),
        required,
    };
    assert_eq!(
        d.forms(),
        vec![Form {
            action: "https://site.test/submit".into(),
            method: "POST".into(),
            fields: vec![
                field("q", "text", None, true),
                field("c", "checkbox", Some("1"), false),
                field("t", "textarea", None, false),
                field("s", "select", None, false),
                field("go", "submit", None, false),
            ],
        }]
    );
}
