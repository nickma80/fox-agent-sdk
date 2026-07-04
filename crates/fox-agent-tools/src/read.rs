use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput, intent_schema_property};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const DEFAULT_LIMIT: usize = 5000;
const MAX_LINE_LEN: usize = 2000;

pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ReadInput {
    file_path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    intent: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadRangeStyle {
    OffsetLimit,
    StartEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NormalizedReadRange {
    offset: usize,
    limit: usize,
    style: ReadRangeStyle,
}

impl NormalizedReadRange {
    fn next_offset(self) -> usize {
        self.offset + self.limit
    }

    fn next_start_line(self) -> usize {
        self.next_offset() + 1
    }
}

fn normalize_read_range(params: &ReadInput) -> Result<NormalizedReadRange, ToolError> {
    let has_start_end = params.start_line.is_some() || params.end_line.is_some();
    let has_mixed_offset = match (params.start_line, params.end_line, params.offset) {
        (Some(start_line), _, Some(offset)) => {
            if start_line == 0 {
                true
            } else {
                offset.checked_add(1) != Some(start_line)
            }
        }
        (None, Some(_), Some(offset)) => offset != 0,
        _ => params.offset.is_some(),
    };

    if has_start_end && has_mixed_offset {
        return Err(ToolError::Message {
            message: "Use either start_line/end_line (1-based) or offset (0-based), not both. `limit` may be used with either style.".to_string(),
        });
    }

    if has_start_end {
        let start_line = params.start_line.unwrap_or(1);
        if start_line == 0 {
            return Err(ToolError::Message {
                message: "start_line must be 1 or greater (it is 1-based).".to_string(),
            });
        }

        let limit = if let Some(end_line) = params.end_line {
            if end_line == 0 {
                return Err(ToolError::Message {
                    message: "end_line must be 1 or greater (it is 1-based).".to_string(),
                });
            }
            if end_line < start_line {
                return Err(ToolError::Message {
                    message: format!(
                        "end_line ({}) must be greater than or equal to start_line ({}).",
                        end_line, start_line
                    ),
                });
            }
            end_line - start_line + 1
        } else {
            params.limit.unwrap_or(DEFAULT_LIMIT)
        };

        return Ok(NormalizedReadRange {
            offset: start_line - 1,
            limit,
            style: ReadRangeStyle::StartEnd,
        });
    }

    Ok(NormalizedReadRange {
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(DEFAULT_LIMIT),
        style: ReadRangeStyle::OffsetLimit,
    })
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file. Supports text files and image files (PNG/JPG/GIF/WebP/BMP)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path"],
            "properties": {
                "intent": intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "Path to a file."
                },
                "start_line": {
                    "type": "integer",
                    "description": "1-based start line for text files."
                },
                "end_line": {
                    "type": "integer",
                    "description": "1-based end line for text files (inclusive)."
                },
                "offset": {
                    "type": "integer",
                    "description": "0-based offset (use instead of start_line if preferred)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max text lines to read. Default 5000."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: ReadInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid read input: {e}"),
        })?;
        let range = normalize_read_range(&params)?;

        let path = ctx.resolve_path(Path::new(&params.file_path));

        // Check if file exists
        if !path.exists() {
            let suggestions = find_similar_files(&path);
            if suggestions.is_empty() {
                return Err(ToolError::Message {
                    message: format!("File not found: {}", params.file_path),
                });
            } else {
                return Err(ToolError::Message {
                    message: format!(
                        "File not found: {}\nDid you mean: {}",
                        params.file_path,
                        suggestions.join(", ")
                    ),
                });
            }
        }

        // Check for image files
        if is_image_file(&path) {
            return handle_image_file(&path, &params.file_path);
        }

        // Check for binary files
        if is_binary_file(&path) {
            return Ok(ToolOutput {
                text: format!(
                    "Binary file detected: {}\nUse appropriate tools to handle binary files.",
                    params.file_path
                ),
                is_error: false,
                json: None,
            });
        }

        // Read text file
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::Message {
                message: format!("failed to read `{}`: {e}", path.display()),
            })?;

        // Single-pass: count lines while building output
        let mut output = String::with_capacity(range.limit.min(2000) * 80);
        let mut total_lines = 0usize;
        let mut truncated_line_count = 0usize;
        let end_exclusive = range.offset + range.limit;
        {
            use std::fmt::Write;
            for (i, line) in content.lines().enumerate() {
                total_lines = i + 1;
                if i < range.offset {
                    continue;
                }
                if i >= end_exclusive {
                    continue;
                }
                let line_num = i + 1;
                if line.len() > MAX_LINE_LEN {
                    truncated_line_count += 1;
                    let _ = writeln!(
                        output,
                        "{:>5}\t{}...",
                        line_num,
                        truncate_str(line, MAX_LINE_LEN)
                    );
                } else {
                    let _ = writeln!(output, "{:>5}\t{}", line_num, line);
                }
            }
        }

        let end = end_exclusive.min(total_lines);

        // Add continuation hint
        if end < total_lines {
            let continuation_hint = match range.style {
                ReadRangeStyle::OffsetLimit => format!("offset={}", range.next_offset()),
                ReadRangeStyle::StartEnd => format!("start_line={}", range.next_start_line()),
            };
            output.push_str(&format!(
                "\n... {} more lines (use {} to continue)\n",
                total_lines - end,
                continuation_hint
            ));
        }

        if output.is_empty() {
            Ok(ToolOutput {
                text: "(empty file)".to_string(),
                is_error: false,
                json: None,
            })
        } else {
            Ok(ToolOutput {
                text: output,
                is_error: false,
                json: Some(json!({
                    "file_path": params.file_path,
                    "start_line": range.offset + 1,
                    "end_line": end,
                    "total_lines": total_lines,
                    "truncated_line_count": truncated_line_count,
                })),
            })
        }
    }
}

fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    // Find the nearest char boundary before max_len
    let mut idx = max_len;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

fn is_binary_file(path: &Path) -> bool {
    // Check by extension first
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        let binary_exts = [
            "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "zip", "tar", "gz", "bz2", "xz",
            "7z", "rar", "exe", "dll", "so", "dylib", "o", "a", "class", "pyc", "wasm", "mp3",
            "mp4", "avi", "mov", "mkv", "flac", "ogg", "wav",
        ];
        if binary_exts.contains(&ext.as_str()) {
            return true;
        }
    }

    // Read only the first 8KB to check for binary content
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buf = [0u8; 8192];
        if let Ok(n) = file.read(&mut buf) {
            if n > 0 {
                let null_count = buf[..n].iter().filter(|&&b| b == 0).count();
                return null_count > n / 10;
            }
        }
    }

    false
}

fn find_similar_files(path: &Path) -> Vec<String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path.file_name().map(|s| s.to_string_lossy().to_lowercase());

    let mut suggestions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if let Some(ref target) = filename {
                let target_str: &str = target.as_ref();
                if name.contains(target_str) || target_str.contains(&name) {
                    suggestions.push(entry.path().display().to_string());
                    if suggestions.len() >= 3 {
                        break;
                    }
                }
            }
        }
    }

    suggestions
}

/// Check if a file is an image based on extension
fn is_image_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico"
        )
    } else {
        false
    }
}

/// Handle reading an image file - return base64 for model vision
fn handle_image_file(path: &Path, file_path: &str) -> Result<ToolOutput, ToolError> {
    let data = std::fs::read(path).map_err(|e| ToolError::Message {
        message: format!("failed to read image `{}`: {e}", path.display()),
    })?;
    let file_size = data.len() as u64;

    let dimensions = get_image_dimensions_from_data(&data);

    let dim_str = dimensions
        .map(|(w, h)| format!("{}x{}", w, h))
        .unwrap_or_else(|| "unknown".to_string());

    let size_str = if file_size < 1024 {
        format!("{} bytes", file_size)
    } else if file_size < 1024 * 1024 {
        format!("{:.1} KB", file_size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1024.0 / 1024.0)
    };

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let media_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "image/png",
    };

    const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;
    let (output_text, json_data) = if file_size <= MAX_IMAGE_SIZE {
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &data,
        );
        (
            format!(
                "Image: {} ({})\nDimensions: {}\nImage sent to model for vision analysis.",
                file_path, size_str, dim_str
            ),
            json!({
                "type": "image",
                "media_type": media_type,
                "data": b64,
                "file_path": file_path,
                "file_size": file_size,
                "dimensions": dim_str,
            }),
        )
    } else {
        (
            format!(
                "Image: {} ({})\nDimensions: {}\nImage too large for vision (max 20MB).",
                file_path, size_str, dim_str
            ),
            json!({
                "type": "image",
                "media_type": media_type,
                "file_path": file_path,
                "file_size": file_size,
                "dimensions": dim_str,
                "too_large": true,
            }),
        )
    };

    Ok(ToolOutput {
        text: output_text,
        is_error: false,
        json: Some(json_data),
    })
}

/// Get image dimensions from raw data
fn get_image_dimensions_from_data(data: &[u8]) -> Option<(u32, u32)> {
    // PNG: check signature and parse IHDR chunk
    if data.len() > 24 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((width, height));
    }

    // JPEG: look for SOF0/SOF2 markers
    if data.len() > 2 && data[0] == 0xFF && data[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            // SOF0 (baseline) or SOF2 (progressive)
            if marker == 0xC0 || marker == 0xC2 {
                let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((width, height));
            }
            // Skip to next marker
            if i + 3 < data.len() {
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + len;
            } else {
                break;
            }
        }
    }

    // GIF: parse header
    if data.len() > 10 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        let width = u16::from_le_bytes([data[6], data[7]]) as u32;
        let height = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Some((width, height));
    }

    None
}
