use diet_code_core::confidence::Confidence;

pub fn header(title: &str) {
    println!("\n🥗 Diet Code\n");
    if !title.is_empty() {
        println!("{}\n", title);
    }
}

pub fn confidence_label(c: &Confidence) -> &'static str {
    match c {
        Confidence::Certain => "CERTAIN",
        Confidence::High => "HIGH",
        Confidence::Medium => "MEDIUM",
        Confidence::Low => "LOW",
    }
}

pub fn print_divider() {
    println!("────────────────────────");
}
