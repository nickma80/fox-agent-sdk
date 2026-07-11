use crate::memory::{RecallHit, RetrievalSource};
use crate::memory::types::{MemoryCategory, MemoryEntry};

/// Format relevant memories as a prompt section.
pub fn format_entries_for_prompt(entries: &[MemoryEntry], limit: usize) -> Option<String> {
    format_entries_for_prompt_with_header(entries, limit, false, false)
}

/// Format with a `# Memory` header.
pub fn format_relevant_prompt(entries: &[MemoryEntry], limit: usize) -> Option<String> {
    format_entries_for_prompt(entries, limit)
        .map(|formatted| format!("# Memory\n\n{formatted}"))
}

/// Format with header and `updated_at` comments (for display/debug).
pub fn format_relevant_display_prompt(entries: &[MemoryEntry], limit: usize) -> Option<String> {
    format_entries_for_prompt_with_header(entries, limit, true, true)
}

pub fn format_recall_hits_prompt(
    hits: &[RecallHit],
    max_chars: usize,
    max_per_category: usize,
) -> Option<String> {
    format_recall_hits(hits, max_chars, max_per_category, false)
}

pub fn format_recall_hits_display_prompt(
    hits: &[RecallHit],
    max_chars: usize,
    max_per_category: usize,
) -> Option<String> {
    format_recall_hits(hits, max_chars, max_per_category, true)
}

pub fn select_recall_hits_for_injection(
    hits: &[RecallHit],
    max_chars: usize,
    max_per_category: usize,
) -> Vec<RecallHit> {
    select_hits(hits, max_chars, max_per_category)
        .into_iter()
        .cloned()
        .collect()
}

pub(super) fn format_entries_for_prompt_with_header(
    entries: &[MemoryEntry],
    limit: usize,
    include_header: bool,
    include_updated_at: bool,
) -> Option<String> {
    let selected = select_entries(entries, limit);
    if selected.is_empty() {
        return None;
    }

    let mut sections: std::collections::BTreeMap<String, Vec<&MemoryEntry>> = std::collections::BTreeMap::new();
    let order = ["corrections", "facts", "preferences", "entities"];

    for entry in &selected {
        let section = match entry.category {
            MemoryCategory::Correction => "corrections",
            MemoryCategory::Fact => "facts",
            MemoryCategory::Preference => "preferences",
            MemoryCategory::Entity => "entities",
            MemoryCategory::Narrative => "narratives",
            MemoryCategory::Custom(ref name) => {
                sections.entry(name.to_lowercase()).or_default().push(entry);
                continue;
            }
        };
        sections.entry(section.to_string()).or_default().push(entry);
    }

    let mut output = String::new();
    let mut first = true;
    for key in order {
        if let Some(items) = sections.remove(key) {
            if !first { output.push('\n'); }
            first = false;
            let title = match key {
                "corrections" => "Corrections",
                "facts" => "Facts",
                "preferences" => "Preferences",
                "entities" => "Entities",
                _ => key,
            };
            output.push_str(&format!("## {title}\n"));
            for (idx, item) in items.iter().enumerate() {
                output.push_str(&format!("{}. {}\n", idx + 1, item.content.trim()));
                if include_updated_at {
                    output.push_str(&format!("<!-- updated_at: {} -->\n", item.updated_at.to_rfc3339()));
                }
            }
        }
    }
    // Remaining custom sections
    for (name, items) in sections {
        if !first { output.push('\n'); }
        first = false;
        output.push_str(&format!("## {name}\n"));
        for (idx, item) in items.iter().enumerate() {
            output.push_str(&format!("{}. {}\n", idx + 1, item.content.trim()));
            if include_updated_at {
                output.push_str(&format!("<!-- updated_at: {} -->\n", item.updated_at.to_rfc3339()));
            }
        }
    }

    if output.is_empty() {
        None
    } else if include_header {
        Some(format!("# Memory\n\n{}", output.trim()))
    } else {
        Some(output.trim().to_string())
    }
}

fn select_entries<'a>(entries: &'a [MemoryEntry], limit: usize) -> Vec<&'a MemoryEntry> {
    use crate::memory::ranking::top_k_by_ord;
    let deduped: Vec<&MemoryEntry> = entries.iter().filter(|e| e.active).collect();
    top_k_by_ord(
        deduped.into_iter().map(|e| (e, e.updated_at.timestamp_millis())),
        limit,
    )
    .into_iter()
    .map(|(e, _)| e)
    .collect()
}

fn format_recall_hits(
    hits: &[RecallHit],
    max_chars: usize,
    max_per_category: usize,
    include_reasons: bool,
) -> Option<String> {
    let selected = select_hits(hits, max_chars, max_per_category);
    if selected.is_empty() {
        return None;
    }

    let mut sections: std::collections::BTreeMap<String, Vec<&RecallHit>> = std::collections::BTreeMap::new();
    let order = ["corrections", "facts", "preferences", "entities"];
    for hit in &selected {
        let section = match hit.entry.category {
            MemoryCategory::Correction => "corrections",
            MemoryCategory::Fact => "facts",
            MemoryCategory::Preference => "preferences",
            MemoryCategory::Entity => "entities",
            MemoryCategory::Narrative => "narratives",
            MemoryCategory::Custom(ref name) => {
                sections.entry(name.to_lowercase()).or_default().push(hit);
                continue;
            }
        };
        sections.entry(section.to_string()).or_default().push(hit);
    }

    let mut output = String::from("# Memory\n\n");
    let mut first = true;
    for key in order {
        if let Some(items) = sections.remove(key) {
            if !first {
                output.push('\n');
            }
            first = false;
            let title = match key {
                "corrections" => "Corrections",
                "facts" => "Facts",
                "preferences" => "Preferences",
                "entities" => "Entities",
                _ => key,
            };
            output.push_str(&format!("## {title}\n"));
            for (idx, hit) in items.iter().enumerate() {
                output.push_str(&format!("{}. {}\n", idx + 1, hit.entry.content.trim()));
                if include_reasons {
                    output.push_str(&format!("   reason: {}\n", explain_hit(hit)));
                }
            }
        }
    }
    for (name, items) in sections {
        if !first {
            output.push('\n');
        }
        first = false;
        output.push_str(&format!("## {name}\n"));
        for (idx, hit) in items.iter().enumerate() {
            output.push_str(&format!("{}. {}\n", idx + 1, hit.entry.content.trim()));
            if include_reasons {
                output.push_str(&format!("   reason: {}\n", explain_hit(hit)));
            }
        }
    }
    Some(output.trim().to_string())
}

fn select_hits<'a>(hits: &'a [RecallHit], max_chars: usize, max_per_category: usize) -> Vec<&'a RecallHit> {
    let mut ordered: Vec<&RecallHit> = hits.iter().filter(|hit| hit.entry.active).collect();
    ordered.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected = Vec::new();
    let mut used_chars = 0usize;
    let mut per_category: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for hit in ordered {
        let category_key = category_key(&hit.entry.category);
        let count = per_category.get(&category_key).copied().unwrap_or(0);
        if count >= max_per_category {
            continue;
        }
        let extra = hit.entry.content.trim().len() + 8;
        if !selected.is_empty() && used_chars + extra > max_chars {
            continue;
        }
        used_chars += extra;
        per_category.insert(category_key, count + 1);
        selected.push(hit);
    }
    selected
}

fn category_key(category: &MemoryCategory) -> String {
    match category {
        MemoryCategory::Correction => "corrections".to_string(),
        MemoryCategory::Fact => "facts".to_string(),
        MemoryCategory::Preference => "preferences".to_string(),
        MemoryCategory::Entity => "entities".to_string(),
        MemoryCategory::Narrative => "narratives".to_string(),
        MemoryCategory::Custom(name) => name.to_lowercase(),
    }
}

fn explain_hit(hit: &RecallHit) -> String {
    let source = match hit.retrieval_source {
        RetrievalSource::Recent => "recent",
        RetrievalSource::Keyword => "keyword",
        RetrievalSource::Semantic => "semantic",
        RetrievalSource::SemanticAnn => "semantic-ann",
        RetrievalSource::CascadeSeed => "cascade-seed",
        RetrievalSource::CascadeGraph => "cascade-graph",
    };
    let mut parts = vec![format!("source={source}")];
    if let Some(score) = hit.score_breakdown.semantic_score {
        parts.push(format!("semantic={score:.2}"));
    }
    if let Some(score) = hit.score_breakdown.keyword_score {
        parts.push(format!("keyword={score:.2}"));
    }
    if let Some(score) = hit.score_breakdown.graph_score {
        parts.push(format!("graph={score:.2}"));
    }
    parts.push(format!("recency={:.2}", hit.score_breakdown.recency_score));
    parts.push(format!("trust={:.2}", hit.score_breakdown.trust_score));
    parts.push(format!("final={:.2}", hit.score_breakdown.final_score));
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{RecallHit, RetrievalSource, ScoreBreakdown, TrustLevel};
    use chrono::Utc;

    fn hit(category: MemoryCategory, content: &str, score: f32) -> RecallHit {
        let mut entry = MemoryEntry::new(category, content);
        entry.trust = TrustLevel::High;
        entry.updated_at = Utc::now();
        RecallHit {
            entry,
            score,
            score_breakdown: ScoreBreakdown {
                semantic_score: Some(score),
                recency_score: 0.8,
                trust_score: 1.0,
                final_score: score,
                ..Default::default()
            },
            retrieval_source: RetrievalSource::Semantic,
        }
    }

    #[test]
    fn recall_hits_prompt_respects_category_and_char_budget() {
        let hits = vec![
            hit(MemoryCategory::Fact, "fact one with some text", 0.99),
            hit(MemoryCategory::Fact, "fact two with some more text", 0.95),
            hit(MemoryCategory::Preference, "pref one", 0.9),
        ];
        let selected = select_recall_hits_for_injection(&hits, 40, 1);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].entry.content, "fact one with some text");
    }

    #[test]
    fn display_prompt_includes_reason_summary() {
        let hits = vec![hit(MemoryCategory::Preference, "prefer concise rust", 0.97)];
        let display = format_recall_hits_display_prompt(&hits, 200, 2).unwrap();
        assert!(display.contains("reason: source=semantic"));
        assert!(display.contains("final="));
    }
}
