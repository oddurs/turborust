//! The turborust site, as a static site generator.
//!
//! Small on purpose. It renders `content/*.md` into `dist/`, wraps each page in
//! one template, and copies `static/` across. That is the whole job — a site of
//! this size does not need a framework, and a framework would obscure the thing
//! this example exists to show: a Rust build step with declared inputs and
//! outputs, so turborust can cache it and explain it.

use pulldown_cmark::{Options, Parser, html};
use std::fs;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist = root.join("dist");

    // Rebuilt rather than merged, so a deleted page actually disappears.
    let _ = fs::remove_dir_all(&dist);
    fs::create_dir_all(&dist)?;

    copy_tree(&root.join("static"), &dist)?;

    let mut pages = Vec::new();
    for entry in fs::read_dir(root.join("content"))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        pages.push(render(&path, &dist)?);
    }
    pages.sort();

    println!("built {} page(s): {}", pages.len(), pages.join(", "));
    Ok(())
}

/// One page: front matter, body, template.
fn render(path: &Path, dist: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(path)?;
    let (front, body) = split_front_matter(&raw);

    let title = front_value(&front, "title").unwrap_or_else(|| "turborust".into());
    let description = front_value(&front, "description").unwrap_or_default();
    let slug = path.file_stem().unwrap().to_string_lossy().to_string();

    // Raw HTML passes through, which is what lets a page carry a hero the
    // markdown itself could never express.
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    let mut content = String::new();
    html::push_html(&mut content, Parser::new_ext(&body, options));

    let page = template(&title, &description, &slug, &content);
    let name = if slug == "index" { "index.html".to_string() } else { format!("{slug}.html") };
    fs::write(dist.join(&name), page)?;
    Ok(name)
}

/// Splits a leading `---` delimited block from the body.
fn split_front_matter(raw: &str) -> (String, String) {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return (String::new(), raw.to_string());
    };
    match rest.split_once("\n---\n") {
        Some((front, body)) => (front.to_string(), body.to_string()),
        None => (String::new(), raw.to_string()),
    }
}

fn front_value(front: &str, key: &str) -> Option<String> {
    front.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

fn template(title: &str, description: &str, slug: &str, content: &str) -> String {
    let nav_docs = if slug == "docs" { " aria-current=\"page\"" } else { "" };
    let nav_home = if slug == "index" { " aria-current=\"page\"" } else { "" };
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<meta name="description" content="{description}">
<link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><text y='.9em' font-size='90'>🦀</text></svg>">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wdth,wght@12..96,75..100,400..800&family=Instrument+Sans:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap">
<link rel="stylesheet" href="/site.css">
</head>
<body>
<a class="skip" href="#main">Skip to content</a>
<header class="topbar">
  <a class="wordmark" href="/"{nav_home}>
    <span class="wordmark-crab" aria-hidden="true">{crab}</span>
    turborust
  </a>
  <nav>
    <a href="/docs.html"{nav_docs}>Docs</a>
    <a href="https://github.com/oddurs/turborust">GitHub</a>
  </nav>
</header>
<main id="main">
{content}
</main>
<footer>
  <p>MIT or Apache-2.0. Built by a Rust generator, served by turborust.</p>
</footer>
<script src="/site.js" defer></script>
</body>
</html>
"##,
        crab = CRAB_MARK,
    )
}

/// A small crab, drawn rather than an emoji: it has to sit on a baseline next to
/// text and take the accent colour, and an emoji does neither.
const CRAB_MARK: &str = r#"<svg viewBox="0 0 24 20" width="22" height="18" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round">
<path d="M4 9 Q2 6 3.5 4"/><path d="M20 9 Q22 6 20.5 4"/>
<ellipse cx="12" cy="11" rx="6.5" ry="4.6" fill="currentColor" stroke="none"/>
<path d="M5.6 13.5 3 16M18.4 13.5 21 16M7 15 6 17.6M17 15 18 17.6"/>
<circle cx="9.8" cy="8.4" r="1.15" fill="var(--shell)" stroke="none"/>
<circle cx="14.2" cy="8.4" r="1.15" fill="var(--shell)" stroke="none"/>
</svg>"#;

/// Copies a directory tree, creating the destination as needed.
fn copy_tree(from: &Path, to: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !from.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&dest)?;
            copy_tree(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}
