//! In-memory compact directory (`MemoryIndex`) for wiki recall and page export.
//!
//! The index is a lossy projection of the graph: id + title + summary + tags +
//! aliases only (no content).  It is rebuilt on demand ([`MemoryIndex::from_graph`])
//! and serialized to `{graph}.index.json` as part of Phase 5.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::memory::graph::MemoryGraph;
use crate::memory::wiki::QueryExpansion;

/// A single index row for a memory entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Compact in-memory directory of all memories in a graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndex {
    pub entries: Vec<IndexEntry>,
    pub updated_at: DateTime<Utc>,
}

impl MemoryIndex {
    /// Build the index from a graph in O(n).
    pub fn from_graph(graph: &MemoryGraph) -> Self {
        let entries = graph
            .all_memories()
            .map(|m| IndexEntry {
                id: m.id.clone(),
                title: m.title.clone(),
                summary: m.summary.clone(),
                tags: m.tags.clone(),
                aliases: m.aliases.clone(),
            })
            .collect();
        Self {
            entries,
            updated_at: Utc::now(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// All current wiki titles (context for `enrich` existing-titles).
    pub fn all_titles(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|e| e.title.clone())
            .collect()
    }

    /// Find the entry whose title equals `title` after normalization.
    pub fn find_by_title(&self, title: &str) -> Option<&IndexEntry> {
        let nt = crate::memory::types::normalize_search_text(title);
        self.entries.iter().find(|e| {
            e.title
                .as_deref()
                .map(crate::memory::types::normalize_search_text)
                == Some(nt.clone())
        })
    }

    /// Weighted lexical score per PRD §5.2 step ②, normalized to [0,1].
    ///
    /// Each query term contributes at most its best field weight:
    /// title 3.0 / aliases 2.0 / tags 1.5 / summary 1.0.  The total is divided
    /// by `3.0 * n` so a query whose terms all hit titles scores 1.0.
    pub fn lexical_score(&self, entry_id: &str, exp: &QueryExpansion) -> f32 {
        let Some(entry) = self.entries.iter().find(|e| e.id == entry_id) else {
            return 0.0;
        };
        let title_text = entry.title.as_deref().unwrap_or("");
        let summary_text = entry.summary.as_deref().unwrap_or("");

        let mut score = 0.0f32;
        for term in exp.all_search_terms() {
            let t = term.to_lowercase();
            let in_title = !title_text.is_empty() && title_text.to_lowercase().contains(&t);
            let in_alias = entry.aliases.iter().any(|a| a.to_lowercase().contains(&t));
            let in_tag = entry.tags.iter().any(|tag| tag.to_lowercase().contains(&t));
            let in_summary = !summary_text.is_empty() && summary_text.to_lowercase().contains(&t);
            if in_title {
                score += 3.0;
            } else if in_alias {
                score += 2.0;
            } else if in_tag {
                score += 1.5;
            } else if in_summary {
                score += 1.0;
            }
        }
        let n = exp.all_search_terms().len().max(1) as f32;
        (score / (3.0 * n)).min(1.0)
    }

    /// Compact llms.txt-style listing for LLM injection, trimmed to `budget_chars`.
    pub fn to_prompt(&self, budget_chars: usize) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let header = "# Memory Index\n";
        if budget_chars < header.len() {
            return None;
        }
        let mut out = String::from(header);
        for (included, e) in self.entries.iter().enumerate() {
            let title = e.title.clone().unwrap_or_else(|| e.id.clone());
            let summary = e.summary.clone().unwrap_or_default();
            let line = format!("- {title}: {summary}\n");
            if out.len() + line.len() > budget_chars {
                let remaining = self.entries.len() - included;
                if remaining > 0 {
                    out.push_str(&format!("... ({remaining} more entries)\n"));
                }
                return Some(out);
            }
            out.push_str(&line);
        }
        Some(out)
    }

    /// Full `index.md` export with wiki links to per-entry pages.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Memory Index\n\n");
        out.push_str(&format!(
            "Updated: {}\n\n",
            self.updated_at.format("%Y-%m-%d %H:%M UTC")
        ));
        for e in &self.entries {
            let title = e.title.clone().unwrap_or_else(|| e.id.clone());
            let summary = e.summary.clone().unwrap_or_default();
            let link = format!("[{}]({})", title, Self::page_path(&slugify(&title)));
            if summary.is_empty() {
                out.push_str(&format!("- {link}\n"));
            } else {
                out.push_str(&format!("- {link} — {summary}\n"));
            }
        }
        out
    }

    /// Relative path for a single memory page (used by index.md export).
    pub fn page_path(slug: &str) -> String {
        format!("pages/{slug}.md")
    }
}

/// Convert a title into a filesystem-safe, URL-friendly slug.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_was_sep = true;
    for ch in title.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('-');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::graph::MemoryGraph;
    use crate::memory::types::{MemoryCategory, MemoryEntry};

    fn entry_with_meta(id: &str, title: &str, summary: &str) -> MemoryEntry {
        let mut e = MemoryEntry::new(MemoryCategory::Fact, format!("content of {id}"));
        e.id = id.to_string();
        e.title = Some(title.to_string());
        e.summary = Some(summary.to_string());
        e
    }

    fn graph_with(entries: Vec<MemoryEntry>) -> MemoryGraph {
        let mut g = MemoryGraph::new();
        for e in entries {
            g.add_memory(e);
        }
        g
    }

    #[test]
    fn index_builds_from_graph_in_memory_order() {
        let g = graph_with(vec![
            entry_with_meta("a", "Rust errors", "Handle errors with Result"),
            entry_with_meta("b", "Python", "Walkthroughs preferred"),
        ]);
        let idx = MemoryIndex::from_graph(&g);
        assert_eq!(idx.len(), 2);
        let mut titles = idx.all_titles();
        titles.sort();
        assert_eq!(titles, vec!["Python", "Rust errors"]);
        assert!(idx.find_by_title("rust errors").is_some());
        assert!(idx.find_by_title("missing").is_none());
    }

    #[test]
    fn index_is_empty_for_empty_graph() {
        assert!(MemoryIndex::from_graph(&MemoryGraph::new()).is_empty());
    }

    #[test]
    fn lexical_score_weights_title_over_summary() {
        let g = graph_with(vec![
            entry_with_meta("a", "Rust errors", "unrelated summary"),
            entry_with_meta("b", "unrelated title", "Rust error handling"),
        ]);
        let idx = MemoryIndex::from_graph(&g);
        let exp = QueryExpansion::from_query("rust error");
        let title_hit = idx.lexical_score("a", &exp);
        let summary_hit = idx.lexical_score("b", &exp);
        assert!(title_hit > summary_hit);
        assert!(title_hit > 0.0);
    }

    #[test]
    fn lexical_score_matches_aliases_and_tags() {
        let g = graph_with(vec![entry_with_meta("a", "Title", "summary")]);
        let idx = MemoryIndex::from_graph(&g);
        let alias_exp = QueryExpansion::from_query("rust alias term");
        let mut alias_entry = idx.entries[0].clone();
        alias_entry.aliases = vec!["alias term".to_string()];
        let mut aliased = idx.clone();
        aliased.entries[0] = alias_entry;
        assert!(aliased.lexical_score("a", &alias_exp) > 0.0);
        assert_eq!(idx.lexical_score("a", &alias_exp), 0.0);
    }

    #[test]
    fn lexical_score_returns_zero_for_unknown_id() {
        let idx = MemoryIndex::from_graph(&graph_with(vec![entry_with_meta("a", "T", "S")]));
        assert_eq!(
            idx.lexical_score("missing", &QueryExpansion::from_query("t")),
            0.0
        );
    }

    #[test]
    fn to_prompt_respects_budget_and_notes_truncation() {
        let g = graph_with(vec![
            entry_with_meta("a", "Alpha memory", "alpha summary"),
            entry_with_meta("b", "Beta memory", "beta summary"),
            entry_with_meta("c", "Gamma memory", "gamma summary"),
        ]);
        let idx = MemoryIndex::from_graph(&g);
        let full = idx.to_prompt(10_000).unwrap();
        assert!(full.starts_with("# Memory Index\n"));
        assert!(full.contains("- Alpha memory: alpha summary"));
        assert!(full.contains("- Beta memory: beta summary"));
        assert!(full.contains("- Gamma memory: gamma summary"));

        let tiny = idx.to_prompt(40).unwrap();
        assert!(tiny.contains("more entries"));
    }

    #[test]
    fn to_prompt_returns_none_when_empty_or_budget_too_small() {
        assert!(
            MemoryIndex::from_graph(&MemoryGraph::new())
                .to_prompt(1000)
                .is_none()
        );
        let idx = MemoryIndex::from_graph(&graph_with(vec![entry_with_meta("a", "T", "S")]));
        assert!(idx.to_prompt(4).is_none());
    }

    #[test]
    fn to_markdown_contains_pages_links() {
        let idx = MemoryIndex::from_graph(&graph_with(vec![entry_with_meta(
            "a",
            "Rust Errors!",
            "Use Result",
        )]));
        let md = idx.to_markdown();
        assert!(md.contains("[Rust Errors!](pages/rust-errors.md)"));
        assert!(md.contains("— Use Result"));
    }

    #[test]
    fn index_serialization_roundtrip() {
        let idx =
            MemoryIndex::from_graph(&graph_with(vec![entry_with_meta("a", "Rust", "Errors")]));
        let json = serde_json::to_string(&idx).unwrap();
        let back: MemoryIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.entries[0], idx.entries[0]);
    }

    #[test]
    fn slugify_produces_url_safe_slugs() {
        assert_eq!(slugify("Rust Error Handling"), "rust-error-handling");
        assert_eq!(slugify("  Cargo.toml  "), "cargo-toml");
        assert_eq!(slugify("…中文标题…"), "untitled");
        assert_eq!(slugify(""), "untitled");
    }

    #[test]
    fn page_path_uses_pages_prefix() {
        assert_eq!(
            MemoryIndex::page_path("rust-errors"),
            "pages/rust-errors.md"
        );
    }
}
