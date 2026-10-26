//! Minimal OTLP/HTTP protobuf-JSON wire model for traces.
//!
//! Deliberately avoids `opentelemetry`/`tonic`/`prost` dependencies: the
//! payload is serialized directly into the OTLP JSON shape
//! (`ExportTraceServiceRequest`), which any OTel backend accepts over
//! `POST {endpoint}/v1/traces` with `Content-Type: application/json`.
//!
//! Encoding rules (proto3 JSON mapping as used by OTLP/HTTP):
//! - `traceId` / `spanId` are **hex** strings (32 / 16 chars)
//! - timestamps and `int64` values are strings
//! - span `kind` enum: `INTERNAL=1`, `SERVER=2`, `CLIENT=3`, `PRODUCER=4`, `CONSUMER=5`
//! - span `status.code` enum: `UNSET=0`, `OK=1`, `ERROR=2`

use serde_json::{Value, json};
use uuid::Uuid;

/// Attribute value for a span / resource / event.
#[derive(Debug, Clone, PartialEq)]
pub enum OtValue {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
    StrArray(Vec<String>),
}

impl OtValue {
    /// Render the proto3-JSON value wrapper: `{"stringValue": "…"}` etc.
    fn into_json(self) -> Value {
        match self {
            OtValue::Str(s) => json!({ "stringValue": s }),
            // proto3 JSON encodes int64 as a decimal string.
            OtValue::Int(i) => json!({ "intValue": i.to_string() }),
            OtValue::Double(d) => json!({ "doubleValue": d }),
            OtValue::Bool(b) => json!({ "boolValue": b }),
            OtValue::StrArray(items) => json!({
                "arrayValue": {
                    "values": items
                        .into_iter()
                        .map(|s| json!({ "stringValue": s }))
                        .collect::<Vec<_>>(),
                }
            }),
        }
    }
}

/// Build an OTLP key-value attribute pair.
pub fn attr(key: impl Into<String>, value: OtValue) -> Value {
    json!({ "key": key.into(), "value": value.into_json() })
}

/// OTLP span kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpSpanKind {
    Internal = 1,
    Server = 2,
    Client = 3,
    Producer = 4,
    Consumer = 5,
}

/// OTLP span status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpStatusCode {
    Unset = 0,
    Ok = 1,
    Error = 2,
}

/// A span event (e.g. `gen_ai.evaluation.result`, `fox_agent.*`).
#[derive(Debug, Clone)]
pub struct OtlpEvent {
    pub name: String,
    pub time_unix_nano: u64,
    pub attributes: Vec<(String, OtValue)>,
}

/// A single OTLP trace span.
#[derive(Debug, Clone)]
pub struct OtlpSpan {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: OtlpSpanKind,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub attributes: Vec<(String, OtValue)>,
    pub events: Vec<OtlpEvent>,
    pub status_code: OtlpStatusCode,
    pub status_message: String,
}

impl OtlpSpan {
    fn to_json(&self) -> Value {
        let mut span = json!({
            "traceId": self.trace_id,
            "spanId": self.span_id,
            "name": self.name,
            "kind": self.kind as i32,
            "startTimeUnixNano": self.start_time_unix_nano.to_string(),
            "endTimeUnixNano": self.end_time_unix_nano.to_string(),
            "attributes": self.attributes.iter().map(|(k, v)| attr(k, v.clone())).collect::<Vec<_>>(),
        });
        if let Some(parent) = &self.parent_span_id {
            span["parentSpanId"] = Value::String(parent.clone());
        }
        if !self.events.is_empty() {
            span["events"] = Value::Array(
                self.events
                    .iter()
                    .map(|e| {
                        json!({
                            "timeUnixNano": e.time_unix_nano.to_string(),
                            "name": e.name,
                            "attributes": e.attributes.iter().map(|(k, v)| attr(k, v.clone())).collect::<Vec<_>>(),
                        })
                    })
                    .collect(),
            );
        }
        let mut status = json!({ "code": self.status_code as i32 });
        if !self.status_message.is_empty() {
            status["message"] = Value::String(self.status_message.clone());
        }
        span["status"] = status;
        span
    }
}

/// Serialize a batch of spans into an `ExportTraceServiceRequest` JSON body.
pub fn export_request(
    service_name: &str,
    scope_name: &str,
    scope_version: &str,
    spans: &[OtlpSpan],
) -> Value {
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [attr("service.name", OtValue::Str(service_name.to_string()))],
            },
            "scopeSpans": [{
                "scope": { "name": scope_name, "version": scope_version },
                "spans": spans.iter().map(OtlpSpan::to_json).collect::<Vec<_>>(),
            }],
        }]
    })
}

/// Generate a random 128-bit hex trace id (32 hex chars).
pub fn hex_trace_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// Generate a random 64-bit hex span id (16 hex chars).
pub fn hex_span_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    uuid[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_shapes() {
        assert_eq!(hex_trace_id().len(), 32);
        assert_eq!(hex_span_id().len(), 16);
    }

    #[test]
    fn value_json_shapes() {
        assert_eq!(
            OtValue::Str("x".into()).into_json(),
            json!({ "stringValue": "x" })
        );
        assert_eq!(OtValue::Int(42).into_json(), json!({ "intValue": "42" }));
        assert_eq!(
            OtValue::Double(1.5).into_json(),
            json!({ "doubleValue": 1.5 })
        );
        assert_eq!(
            OtValue::Bool(true).into_json(),
            json!({ "boolValue": true })
        );
        assert_eq!(
            OtValue::StrArray(vec!["a".into(), "b".into()]).into_json(),
            json!({ "arrayValue": { "values": [{ "stringValue": "a" }, { "stringValue": "b" }] } })
        );
    }

    #[test]
    fn span_serializes_with_hex_ids_and_string_ints() {
        let span = OtlpSpan {
            trace_id: hex_trace_id(),
            span_id: hex_span_id(),
            parent_span_id: Some(hex_span_id()),
            name: "chat gpt-4o".to_string(),
            kind: OtlpSpanKind::Client,
            start_time_unix_nano: 1_700_000_000_000_000_000,
            end_time_unix_nano: 1_700_000_000_000_500_000,
            attributes: vec![
                ("gen_ai.operation.name".into(), OtValue::Str("chat".into())),
                ("gen_ai.usage.input_tokens".into(), OtValue::Int(10)),
            ],
            events: vec![OtlpEvent {
                name: "gen_ai.evaluation.result".into(),
                time_unix_nano: 1_700_000_000_000_300_000,
                attributes: vec![
                    (
                        "gen_ai.evaluation.name".into(),
                        OtValue::Str("correctness".into()),
                    ),
                    (
                        "gen_ai.evaluation.score.value".into(),
                        OtValue::Double(0.95),
                    ),
                ],
            }],
            status_code: OtlpStatusCode::Ok,
            status_message: String::new(),
        };

        let body = export_request("fox-agent", "fox-agent-sdk", "0.1.0", &[span]);
        let spans = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(spans["name"], "chat gpt-4o");
        assert_eq!(spans["kind"], 3);
        assert_eq!(spans["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(spans["spanId"].as_str().unwrap().len(), 16);
        assert_eq!(spans["parentSpanId"].as_str().unwrap().len(), 16);
        assert!(spans["startTimeUnixNano"].as_str().is_some());
        assert_eq!(spans["status"]["code"], 1);
        assert_eq!(spans["events"][0]["name"], "gen_ai.evaluation.result");
    }
}
