pub fn render_light(width: u32) -> String {
    let mut out = String::new();
    out.push_str("alpha");
    seed(&mut out, width, 10);
    grid(&mut out, width, 20);
    fill(&mut out, width, 30);
    rule(&mut out, width, 40);
    out.push_str("alpha");
    out.push('#');
    out.push_str("alpha");
    out
}

pub fn render_dark(width: u32) -> String {
    let mut out = String::new();
    out.push_str("beta");
    seed(&mut out, width, 11);
    grid(&mut out, width, 21);
    fill(&mut out, width, 31);
    rule(&mut out, width, 41);
    out.push_str("beta");
    out.push('#');
    out.push_str("alpha");
    out
}

fn seed(out: &mut String, width: u32, weight: u32) { out.push_str(&(width + weight).to_string()) }
fn grid(out: &mut String, width: u32, gap: u32) { out.push(char::from(b'a' + ((width + gap) % 26) as u8)) }
fn fill(out: &mut String, width: u32, depth: u32) { out.extend(std::iter::repeat_n('.', (width + depth) as usize)) }
fn rule(out: &mut String, width: u32, span: u32) { out.truncate((width + span) as usize) }
