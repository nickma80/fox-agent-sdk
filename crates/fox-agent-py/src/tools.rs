//! Python custom tool adapter.
//!
//! Allows Python developers to write custom tools by implementing
//! a simple Python class, then register it with the AgentBuilder.
//!
//! ```python
//! class MyTool:
//!     def name(self) -> str:
//!         return "my_tool"
//!     def description(self) -> str:
//!         return "Does something useful"
//!     def parameters_schema(self) -> dict:
//!         return {"type": "object", "properties": {}}
//!     def execute(self, input: dict, ctx: ToolContext) -> ToolOutput:
//!         return ToolOutput(text="done")
//!
//! builder.with_tool(MyTool())
//! ```

use fox_agent_core::{Tool, ToolContext, ToolError, ToolExecutionMode, ToolOutput};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;
use std::sync::Arc;

// ── PyToolContext ──

/// Python-exposed execution context passed to every tool invocation.
#[pyclass(name = "ToolContext", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyToolContext {
    #[pyo3(get)]
    pub session_id: String,
    #[pyo3(get)]
    pub message_id: String,
    #[pyo3(get)]
    pub tool_call_id: String,
    #[pyo3(get)]
    pub working_dir: Option<String>,
    #[pyo3(get)]
    pub is_background: bool,
    #[pyo3(get)]
    pub graceful_shutdown_requested: bool,
}

impl PyToolContext {
    pub fn from_rust(ctx: &ToolContext) -> Self {
        Self {
            session_id: ctx.session_id.clone(),
            message_id: ctx.message_id.clone(),
            tool_call_id: ctx.tool_call_id.clone(),
            working_dir: ctx
                .working_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            is_background: matches!(ctx.execution_mode, ToolExecutionMode::Background),
            graceful_shutdown_requested: ctx.graceful_shutdown_requested,
        }
    }
}

#[pymethods]
impl PyToolContext {
    fn __repr__(&self) -> String {
        format!(
            "ToolContext(session_id={}, tool_call_id={})",
            self.session_id, self.tool_call_id
        )
    }
}

// ── PyToolOutput ──

/// Python-exposed tool output.
///
/// Python tools return this from their `execute()` method.
#[pyclass(name = "ToolOutput", module = "fox_agent_sdk._core")]
#[derive(Clone)]
pub struct PyToolOutput {
    inner: ToolOutput,
}

#[pymethods]
impl PyToolOutput {
    /// Create a successful tool output with text.
    #[new]
    #[pyo3(signature = (text, *, is_error = false, json = None))]
    fn new(text: String, is_error: bool, json: Option<String>) -> PyResult<Self> {
        let json_val = json
            .map(|s| serde_json::from_str::<Value>(&s))
            .transpose()
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid JSON: {}", e)))?;

        Ok(Self {
            inner: ToolOutput {
                text,
                is_error,
                json: json_val,
            },
        })
    }

    /// Create an error tool output.
    #[staticmethod]
    fn error(text: String) -> Self {
        Self {
            inner: ToolOutput::error(text),
        }
    }

    #[getter]
    fn text(&self) -> &str {
        &self.inner.text
    }

    #[getter]
    fn is_error(&self) -> bool {
        self.inner.is_error
    }

    #[getter]
    fn json(&self) -> Option<String> {
        self.inner.json.as_ref().map(|v| v.to_string())
    }

    fn __repr__(&self) -> String {
        format!(
            "ToolOutput(text='{}...', is_error={})",
            &self.inner.text.chars().take(50).collect::<String>(),
            self.inner.is_error
        )
    }
}

impl PyToolOutput {
    pub fn into_inner(self) -> ToolOutput {
        self.inner
    }
}

impl From<ToolOutput> for PyToolOutput {
    fn from(output: ToolOutput) -> Self {
        Self { inner: output }
    }
}

// ── PyTool adapter ──

/// Python-exposed base class for custom tools.
///
/// Python developers subclass this and override `name`, `description`,
/// `parameters_schema`, and `execute`.
///
/// ```python
/// class WeatherTool:
///     def name(self) -> str:
///         return "get_weather"
///     def description(self) -> str:
///         return "Get current weather"
///     def parameters_schema(self) -> dict:
///         return {"type": "object", "properties": {"city": {"type": "string"}}}
///     def execute(self, input: dict, ctx: ToolContext) -> ToolOutput:
///         city = input["city"]
///         return ToolOutput(text=f"Weather in {city}: sunny")
/// ```
#[pyclass(name = "Tool", module = "fox_agent_sdk._core", subclass)]
pub struct PyTool {
    /// Cached values populated eagerly at construction time via Python calls.
    #[allow(dead_code)]
    cached_name: String,
    #[allow(dead_code)]
    cached_description: String,
    #[allow(dead_code)]
    cached_schema: Value,
}

#[pymethods]
impl PyTool {
    #[new]
    fn new() -> Self {
        Self {
            cached_name: String::new(),
            cached_description: String::new(),
            cached_schema: Value::Object(serde_json::Map::new()),
        }
    }
}

// ── Internal adapter: wraps a PyTool Python object and implements the Rust Tool trait ──

/// Internal adapter that holds a Python tool object and dispatches
/// Rust `Tool` trait calls back to Python.
pub(crate) struct PyToolAdapter {
    /// Reference to the Python tool object.
    py_obj: Py<PyAny>,
    /// Cached metadata so sync methods don't need GIL every call.
    name: String,
    description: String,
    schema: Value,
}

impl PyToolAdapter {
    /// Create an adapter from a Python tool object.
    /// Eagerly calls Python `name()`, `description()`, `parameters_schema()` to cache.
    pub fn new(py_obj: Py<PyAny>) -> PyResult<Self> {
        let name: String = Python::with_gil(|py| {
            let obj = py_obj.bind(py);
            obj.call_method0("name")?.extract()
        })?;

        let description: String = Python::with_gil(|py| {
            let obj = py_obj.bind(py);
            obj.call_method0("description")?.extract()
        })?;

        // Extract parameters_schema as a JSON Value.
        let schema: Value = Python::with_gil(|py| {
            let obj = py_obj.bind(py);
            let result = obj.call_method0("parameters_schema")?;
            let dict: Bound<'_, PyDict> = result.downcast_into()?;
            py_dict_to_json_value(py, &dict)
        })?;

        Ok(Self {
            py_obj,
            name,
            description,
            schema,
        })
    }
}

/// Convert a Python dict recursively into a serde_json::Value.
fn py_dict_to_json_value(py: Python<'_>, dict: &Bound<'_, PyDict>) -> PyResult<Value> {
    let mut map = serde_json::Map::new();
    for (key, value) in dict.iter() {
        let k: String = key.extract()?;
        let v = py_any_to_json_value(py, &value)?;
        map.insert(k, v);
    }
    Ok(Value::Object(map))
}

fn py_any_to_json_value(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if let Ok(s) = obj.extract::<String>() {
        Ok(Value::String(s))
    } else if let Ok(n) = obj.extract::<i64>() {
        Ok(Value::Number(serde_json::Number::from(n)))
    } else if let Ok(n) = obj.extract::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            Ok(Value::Number(num))
        } else {
            Ok(Value::Null)
        }
    } else if let Ok(b) = obj.extract::<bool>() {
        Ok(Value::Bool(b))
    } else if obj.is_none() {
        Ok(Value::Null)
    } else if let Ok(dict) = obj.downcast::<PyDict>() {
        py_dict_to_json_value(py, dict)
    } else if let Ok(list) = obj.downcast::<pyo3::types::PyList>() {
        let mut arr = Vec::new();
        for item in list.iter() {
            arr.push(py_any_to_json_value(py, &item)?);
        }
        Ok(Value::Array(arr))
    } else {
        // Fallback: use Python repr
        let s: String = obj.repr()?.extract()?;
        Ok(Value::String(s))
    }
}

#[async_trait::async_trait]
impl Tool for PyToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        // Clone the Python reference with GIL held (Py<T> doesn't impl Clone).
        let py_obj = Python::with_gil(|py| self.py_obj.clone_ref(py));
        let py_ctx = PyToolContext::from_rust(&ctx);

        // Run the Python call on a blocking thread to avoid blocking the
        // tokio runtime while holding the GIL.
        let result: Result<ToolOutput, ToolError> = tokio::task::spawn_blocking(move || {
            Python::with_gil(|py| {
                let obj = py_obj.bind(py);

                // Convert serde_json::Value input to Python dict
                let input_dict =
                    json_value_to_py_dict(py, &input).map_err(|e| ToolError::Message {
                        message: format!("failed to convert input: {}", e),
                    })?;

                // Convert PyToolContext to Python object
                let ctx_obj = Py::new(py, py_ctx).map_err(|e| ToolError::Message {
                    message: format!("failed to create ToolContext: {}", e),
                })?;

                // Call execute(input, ctx)
                let output = obj
                    .call_method1("execute", (input_dict, ctx_obj))
                    .map_err(|e| ToolError::Message {
                        message: format!("Python tool execute error: {}", e),
                    })?;

                // Extract PyToolOutput from the result
                if let Ok(py_output) = output.extract::<PyToolOutput>() {
                    Ok(py_output.into_inner())
                } else if let Ok(dict) = output.downcast::<PyDict>() {
                    // Graceful fallback: accept a plain dict with text/is_error/json
                    let text: String = dict
                        .get_item("text")
                        .ok()
                        .flatten()
                        .and_then(|v| v.extract().ok())
                        .unwrap_or_default();
                    let is_error: bool = dict
                        .get_item("is_error")
                        .ok()
                        .flatten()
                        .and_then(|v| v.extract().ok())
                        .unwrap_or(false);
                    let json: Option<Value> = dict.get_item("json").ok().flatten().and_then(|v| {
                        let s: String = v.extract().ok()?;
                        serde_json::from_str(&s).ok()
                    });
                    Ok(ToolOutput {
                        text,
                        is_error,
                        json,
                    })
                } else {
                    // Last resort: convert to string
                    let text: String = output.str().map(|s| s.to_string()).unwrap_or_default();
                    Ok(ToolOutput::new(text))
                }
            })
        })
        .await
        .map_err(|e| ToolError::Message {
            message: format!("tool execution panicked: {}", e),
        })?;

        result
    }
}

/// Convert a serde_json::Value to a Python dict.
fn json_value_to_py_dict(py: Python<'_>, value: &Value) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    if let Value::Object(map) = value {
        for (k, v) in map {
            let py_val = json_value_to_py_any(py, v)?;
            dict.set_item(k, py_val)?;
        }
    }
    Ok(dict.into())
}

fn json_value_to_py_any(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    match value {
        Value::Null => Ok(py.None().into()),
        Value::Bool(b) => {
            let py_bool = pyo3::types::PyBool::new(py, *b).to_owned();
            Ok(py_bool.into_any().unbind())
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)?.into())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)?.into())
            } else {
                Ok(py.None().into())
            }
        }
        Value::String(s) => Ok(s.clone().into_pyobject(py)?.into()),
        Value::Array(arr) => {
            let list = pyo3::types::PyList::empty(py);
            for item in arr {
                let py_item = json_value_to_py_any(py, item)?;
                list.append(py_item)?;
            }
            Ok(list.into())
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                let py_val = json_value_to_py_any(py, v)?;
                dict.set_item(k, py_val)?;
            }
            Ok(dict.into())
        }
    }
}

// ── Helper: register a Python tool object as Arc<dyn Tool> ──

/// Register a Python tool object with the builder.
/// Returns Arc<dyn Tool> suitable for `with_tool()`.
pub(crate) fn register_python_tool(py_obj: Py<PyAny>) -> PyResult<Arc<dyn Tool>> {
    let adapter = PyToolAdapter::new(py_obj)?;
    Ok(Arc::new(adapter))
}
