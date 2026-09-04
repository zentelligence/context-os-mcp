use super::*;

#[test]
fn wraps_the_body_and_carries_the_title() {
    let html = render_page("example.md", "<p>Body.</p>");
    assert!(html.contains("<title>example.md</title>"));
    assert!(html.contains("<p>Body.</p>"));
}
