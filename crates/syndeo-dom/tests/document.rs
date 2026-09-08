use syndeo_dom::Document;

const PAGE: &str = r##"
<!doctype html>
<html>
<head>
  <title>  Ledger  </title>
  <link rel="stylesheet" href="/style.css"
        integrity="sha384-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ012345678901234567">
  <script src="https://cdn.test/app.js"
          integrity="sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=" crossorigin="anonymous"></script>
  <style>.hidden { display: none }</style>
</head>
<body>
  <nav><a href="/home">Home</a> <a href="#top">Top</a></nav>
  <h1>Balance</h1>
  <p>You hold  120   SUM.</p>
  <p>Pending: <strong>3</strong> transfers.</p>
  <script>console.log('not prose');</script>
  <img src="chart.png">
  <form action="/transfer" method="post">
    <input type="text" name="recipient" required>
    <input type="number" name="amount" value="10">
    <button type="submit" name="go">Send</button>
  </form>
  <a href="https://other.test/docs" rel="noopener">Docs</a>
</body>
</html>
"##;

fn document() -> Document {
    Document::parse(PAGE, Some("https://ledger.test/account/"))
}

#[test]
fn the_title_is_trimmed() {
    assert_eq!(document().title().as_deref(), Some("Ledger"));
}

#[test]
fn text_is_prose_only_with_whitespace_collapsed() {
    let text = document().text();
    assert!(text.contains("You hold 120 SUM."));
    assert!(text.contains("Pending: 3 transfers."));
    assert!(!text.contains("console.log"), "script bodies are not prose");
    assert!(!text.contains("display: none"), "style bodies are not prose");
}

#[test]
fn blocks_carry_their_element_and_heading_depth() {
    let blocks = document().blocks();
    let heading = blocks.iter().find(|b| b.text == "Balance").unwrap();
    assert_eq!(heading.element, "h1");
    assert_eq!(heading.heading_level, Some(1));
    assert!(blocks.iter().any(|b| b.element == "p" && b.text.contains("120 SUM")));
}

#[test]
fn relative_links_resolve_against_the_base_and_fragments_are_dropped() {
    let links = document().links();
    let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
    assert!(urls.contains(&"https://ledger.test/home"));
    assert!(urls.contains(&"https://other.test/docs"));
    assert!(!urls.iter().any(|u| u.contains('#')), "same-page anchors are not navigation");
}

#[test]
fn subresources_are_found_with_their_kind() {
    let resources = document().subresources();
    let by_kind = |kind: &str| {
        resources
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.url.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(by_kind("stylesheet"), vec!["https://ledger.test/style.css"]);
    assert_eq!(by_kind("script"), vec!["https://cdn.test/app.js"]);
    assert_eq!(by_kind("image"), vec!["https://ledger.test/account/chart.png"]);
}

#[test]
fn integrity_metadata_is_parsed_and_keyed_by_resolved_url() {
    let map = document().integrity_map();
    let (url, integrity) = map
        .iter()
        .find(|(u, _)| u == "https://cdn.test/app.js")
        .expect("the script declares integrity");
    assert_eq!(url, "https://cdn.test/app.js");
    // The sha256 in the fixture is the digest of the empty body.
    assert!(integrity.verify(b""));
    assert!(!integrity.verify(b"tampered"));
    // The image declares nothing, so it contributes no hash and can never be
    // taken from a peer on a first fetch.
    assert!(!map.iter().any(|(u, _)| u.ends_with("chart.png")));
}

#[test]
fn forms_are_described_well_enough_to_fill_in() {
    let forms = document().forms();
    assert_eq!(forms.len(), 1);
    let form = &forms[0];
    assert_eq!(form.action, "https://ledger.test/transfer");
    assert_eq!(form.method, "POST");

    let recipient = form.fields.iter().find(|f| f.name == "recipient").unwrap();
    assert_eq!(recipient.kind, "text");
    assert!(recipient.required);

    let amount = form.fields.iter().find(|f| f.name == "amount").unwrap();
    assert_eq!(amount.value.as_deref(), Some("10"));
    assert!(!amount.required);
}

#[test]
fn malformed_markup_still_parses() {
    let doc = Document::parse("<p>unclosed <b>bold<p>next", Some("https://a.test/"));
    assert!(doc.text().contains("unclosed"));
    assert!(doc.text().contains("next"));
}
