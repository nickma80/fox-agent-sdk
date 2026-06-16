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
