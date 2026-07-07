pub fn warm_label(count: u32) -> String {
    let mut text = String::with_capacity(64);
    text.push_str("aurora");
    text.push(':');
    text.push_str(&count.to_string());
    text.push_str(" via ");
    text.push_str("aurora");
    text.trim().to_string()
}

pub fn cool_label(total: u32) -> String {
    let mut text = String::with_capacity(64);
    text.push_str("borealis");
    text.push(':');
    text.push_str(&total.to_string());
    text.push_str(" via ");
    text.push_str("borealis");
    text.trim().to_string()
}
