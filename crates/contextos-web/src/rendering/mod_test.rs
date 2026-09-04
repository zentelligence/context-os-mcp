use super::*;

#[test]
fn escapes_every_html_special_character() {
    assert_eq!(
        escape_html(r#"<script>&"'</script>"#),
        "&lt;script&gt;&amp;&quot;&#39;&lt;/script&gt;"
    );
}

#[test]
fn leaves_plain_text_unchanged() {
    assert_eq!(escape_html("plain text"), "plain text");
}
