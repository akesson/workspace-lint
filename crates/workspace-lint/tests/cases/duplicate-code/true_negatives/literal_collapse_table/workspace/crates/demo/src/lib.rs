pub enum Keyword {
    Fn,
    Let,
    Match,
    Loop,
    Impl,
    Trait,
}

pub fn short_name(kw: &Keyword) -> &'static str {
    match kw {
        Keyword::Fn => "fn",
        Keyword::Let => "let",
        Keyword::Match => "match",
        Keyword::Loop => "loop",
        Keyword::Impl => "impl",
        Keyword::Trait => "trait",
    }
}

pub fn describe(kw: &Keyword) -> &'static str {
    match kw {
        Keyword::Fn => "a function definition",
        Keyword::Let => "a local binding",
        Keyword::Match => "a pattern match",
        Keyword::Loop => "an infinite loop",
        Keyword::Impl => "an implementation block",
        Keyword::Trait => "a trait declaration",
    }
}
