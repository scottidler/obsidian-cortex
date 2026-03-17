use std::collections::HashMap;
use tracing::instrument;

use crate::config::DuplicatesConfig;
use crate::report::{Report, Severity, Violation};
use crate::vault::Note;

/// Run duplicate detection on all notes.
#[instrument(skip(notes, config))]
pub fn lint_duplicates(notes: &[Note], config: &DuplicatesConfig) -> Report {
    let mut report = Report::default();

    // Phase 1: exact content hash duplicates
    let mut hash_groups: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, note) in notes.iter().enumerate() {
        let hash = simple_hash(&note.body);
        hash_groups.entry(hash).or_default().push(i);
    }

    for indices in hash_groups.values() {
        if indices.len() < 2 {
            continue;
        }

        // Check same-type-only constraint
        if config.same_type_only {
            let types: Vec<Option<&str>> = indices
                .iter()
                .map(|&i| notes[i].frontmatter.note_type.as_deref())
                .collect();
            if !types.windows(2).all(|w| w[0] == w[1]) {
                continue;
            }
        }

        let first = &notes[indices[0]];
        for &idx in &indices[1..] {
            let dupe = &notes[idx];
            report.add(Violation {
                path: dupe.path.clone(),
                rule: "duplicates.exact".to_string(),
                severity: Severity::Warning,
                message: format!("exact duplicate of {}", first.path.display()),
                fix: None,
            });
        }
    }

    // Phase 2: fuzzy similarity (TF-IDF based)
    if notes.len() > 1 && config.threshold < 1.0 {
        let similarities = find_similar_notes(notes, config.threshold, config.same_type_only);
        for (i, j, score) in similarities {
            // Skip if already reported as exact duplicate
            let already_reported = report
                .violations
                .iter()
                .any(|v| v.path == notes[j].path && v.rule == "duplicates.exact");
            if already_reported {
                continue;
            }

            report.add(Violation {
                path: notes[j].path.clone(),
                rule: "duplicates.similar".to_string(),
                severity: Severity::Info,
                message: format!("similar to {} (score: {:.2})", notes[i].path.display(), score),
                fix: None,
            });
        }
    }

    tracing::info!(violation_count = report.violations.len(), "duplicates lint complete");
    report
}

/// Simple non-cryptographic hash for body content comparison.
fn simple_hash(content: &str) -> u64 {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return 0;
    }
    // FNV-1a hash
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in trimmed.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Build a simple TF-IDF model and find similar note pairs.
fn find_similar_notes(notes: &[Note], threshold: f64, same_type_only: bool) -> Vec<(usize, usize, f64)> {
    let tokenized: Vec<HashMap<&str, usize>> = notes.iter().map(|n| tokenize(&n.body)).collect();

    // Document frequency
    let mut df: HashMap<&str, usize> = HashMap::new();
    for tokens in &tokenized {
        for term in tokens.keys() {
            *df.entry(term).or_insert(0) += 1;
        }
    }

    let n = notes.len() as f64;

    // TF-IDF vectors (sparse)
    let tfidf_vecs: Vec<HashMap<&str, f64>> = tokenized
        .iter()
        .map(|tokens| {
            let total: usize = tokens.values().sum();
            if total == 0 {
                return HashMap::new();
            }
            tokens
                .iter()
                .map(|(term, &count)| {
                    let tf = count as f64 / total as f64;
                    let idf = (n / (*df.get(term).unwrap_or(&1)) as f64).ln() + 1.0;
                    (*term, tf * idf)
                })
                .collect()
        })
        .collect();

    let mut results = Vec::new();

    for i in 0..notes.len() {
        for j in (i + 1)..notes.len() {
            if same_type_only && notes[i].frontmatter.note_type != notes[j].frontmatter.note_type {
                continue;
            }

            let score = cosine_similarity(&tfidf_vecs[i], &tfidf_vecs[j]);
            if score >= threshold {
                results.push((i, j, score));
            }
        }
    }

    results
}

/// Tokenize text into word frequency map.
fn tokenize(text: &str) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.len() >= 2 {
            *counts.entry(word).or_insert(0) += 1;
        }
    }
    counts
}

/// Cosine similarity between two sparse TF-IDF vectors.
fn cosine_similarity(a: &HashMap<&str, f64>, b: &HashMap<&str, f64>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let dot: f64 = a
        .iter()
        .filter_map(|(term, val_a)| b.get(term).map(|val_b| val_a * val_b))
        .sum();

    let mag_a: f64 = a.values().map(|v| v * v).sum::<f64>().sqrt();
    let mag_b: f64 = b.values().map(|v| v * v).sum::<f64>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }

    dot / (mag_a * mag_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::Frontmatter;
    use std::path::PathBuf;

    fn make_note(path: &str, body: &str, note_type: Option<&str>) -> Note {
        Note {
            path: PathBuf::from(path),
            frontmatter: Frontmatter {
                title: Some(path.to_string()),
                note_type: note_type.map(String::from),
                ..Default::default()
            },
            body: body.to_string(),
            raw: String::new(),
        }
    }

    #[test]
    fn test_exact_duplicates() {
        let notes = vec![
            make_note("a.md", "Hello world this is a test note.", None),
            make_note("b.md", "Hello world this is a test note.", None),
        ];

        let config = DuplicatesConfig {
            threshold: 0.85,
            same_type_only: false,
        };

        let report = lint_duplicates(&notes, &config);
        assert!(report.violations.iter().any(|v| v.rule == "duplicates.exact"));
    }

    #[test]
    fn test_no_duplicates() {
        let notes = vec![
            make_note("a.md", "Completely different content about Rust.", None),
            make_note("b.md", "This note discusses cooking recipes for pasta.", None),
        ];

        let config = DuplicatesConfig {
            threshold: 0.85,
            same_type_only: false,
        };

        let report = lint_duplicates(&notes, &config);
        assert!(report.is_empty());
    }

    #[test]
    fn test_similar_notes() {
        let notes = vec![
            make_note(
                "a.md",
                "Rust programming language systems programming memory safety borrow checker",
                None,
            ),
            make_note(
                "b.md",
                "Rust programming language systems programming memory safety ownership",
                None,
            ),
        ];

        let config = DuplicatesConfig {
            threshold: 0.5,
            same_type_only: false,
        };

        let report = lint_duplicates(&notes, &config);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == "duplicates.similar" || v.rule == "duplicates.exact")
        );
    }

    #[test]
    fn test_same_type_only() {
        let notes = vec![
            make_note("a.md", "Same content here.", Some("video")),
            make_note("b.md", "Same content here.", Some("article")),
        ];

        let config = DuplicatesConfig {
            threshold: 0.85,
            same_type_only: true,
        };

        let report = lint_duplicates(&notes, &config);
        assert!(report.is_empty());
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let mut a = HashMap::new();
        a.insert("hello", 1.0);
        a.insert("world", 1.0);

        let score = cosine_similarity(&a, &a);
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let mut a = HashMap::new();
        a.insert("hello", 1.0);

        let mut b = HashMap::new();
        b.insert("world", 1.0);

        let score = cosine_similarity(&a, &b);
        assert!((score - 0.0).abs() < 0.001);
    }
}
