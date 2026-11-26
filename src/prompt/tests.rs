use super::*;

fn hash(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

fn request() -> Value {
    json!({"system": "S", "tools": [], "stable": 0, "messages": [
        {"role": "user", "blocks": [{"type": "text", "text": "old"}, {"type": "image", "blob": hash('a'), "media_type": "image/png", "width": 1, "height": 1}]},
        {"role": "assistant", "blocks": [{"type": "text", "text": "seen"}]},
        {"role": "user", "blocks": [
            {"type": "text", "text": "look"},
            {"type": "image", "blob": hash('b'), "name": "cat.png", "media_type": "image/png", "width": 1, "height": 1},
            {"type": "file", "blob": hash('c'), "name": "a b.pdf", "media_type": "application/pdf"},
            {"type": "file", "blob": hash('d'), "name": "voice.mp3", "media_type": "audio/mpeg"}
        ]}
    ]})
}

fn blobs() -> Map<String, Value> {
    [
        ('a', "image/png"),
        ('b', "image/png"),
        ('c', "application/pdf"),
        ('d', "audio/mpeg"),
    ]
    .into_iter()
    .map(|(c, kind)| {
        (
            hash(c),
            json!({"media_type": kind, "data": format!("{c}{c}")}),
        )
    })
    .collect()
}

#[test]
fn a_fresh_prompt_attaches_only_this_turn_s_media_the_agent_takes() {
    let blobs = blobs();
    let media = Media {
        blobs: Some(&blobs),
        image: true,
        audio: false,
        embedded: true,
    };
    let prompt = fresh(&request(), &media, Some("SYS")).unwrap();
    let body = prompt[0]["text"].as_str().unwrap();
    assert!(
        body.starts_with("<system-instructions>\nSYS\n</system-instructions>\n\n"),
        "{body}"
    );
    assert!(body.contains("[image omitted"), "历史里的图不附：{body}");
    assert!(
        body.contains("[image \"cat.png\" attached below]"),
        "{body}"
    );
    assert!(body.contains("[file \"a b.pdf\" attached below]"), "{body}");
    assert!(
        body.contains("[file \"voice.mp3\" attached below]"),
        "不收音频的当嵌入的资源交：{body}"
    );
    assert_eq!(
        prompt[1],
        json!({"type": "image", "mimeType": "image/png", "data": "bb"})
    );
    assert_eq!(
        prompt[2],
        json!({"type": "resource", "resource": {"uri": "attachment:///a%20b.pdf", "mimeType": "application/pdf", "blob": "cc"}})
    );
    assert_eq!(prompt[3]["resource"]["mimeType"], "audio/mpeg");
    assert_eq!(prompt.len(), 4);
}

#[test]
fn without_capabilities_nothing_is_attached_and_without_system_there_is_no_shell() {
    let blobs = blobs();
    let media = Media {
        blobs: Some(&blobs),
        image: false,
        audio: false,
        embedded: false,
    };
    let prompt = fresh(&request(), &media, None).unwrap();
    assert_eq!(prompt.len(), 1);
    assert!(
        !prompt[0]["text"]
            .as_str()
            .unwrap()
            .contains("system-instructions")
    );
}

#[test]
fn the_next_turn_sends_new_messages_and_their_media() {
    let blobs = blobs();
    let media = Media {
        blobs: Some(&blobs),
        image: true,
        audio: true,
        embedded: false,
    };
    let mut req = request();
    req["tail"] = json!("<tail/>");
    let prompt = next(&req, 2, &media).unwrap();
    let body = prompt[0]["text"].as_str().unwrap();
    assert!(
        body.starts_with("look\n[image \"cat.png\" attached below]"),
        "{body}"
    );
    assert!(body.ends_with("<tail/>"), "{body}");
    assert_eq!(prompt[1]["type"], "image");
    assert!(
        body.contains("[file \"a b.pdf\" omitted"),
        "不收嵌入资源的写占位：{body}"
    );
    assert_eq!(
        prompt[2],
        json!({"type": "audio", "mimeType": "audio/mpeg", "data": "dd"})
    );
    assert_eq!(prompt.len(), 3);
}
