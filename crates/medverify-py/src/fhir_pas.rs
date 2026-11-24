use crate::{decode, extract, output};
use jiff::{civil::Date, Timestamp, Zoned};
use medverify_core::fhir_pas as core;
use pyo3::{
    exceptions::{PyTypeError, PyValueError},
    prelude::*,
};
use serde::Deserialize;
use std::collections::BTreeMap;

#[pyclass(frozen, module = "medverify.fhir_pas")]
struct ValidatedRequest(core::Request<'static>);

#[pymethods]
impl ValidatedRequest {
    #[getter]
    fn sequences(&self) -> Vec<u32> {
        self.0.items().map(|item| item.sequence).collect()
    }

    #[getter]
    fn priority<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        output(py, self.0.priority())
    }
}

#[pyclass(frozen, module = "medverify.fhir_pas")]
struct Response(core::Response);

#[pymethods]
impl Response {
    fn action<'py>(&self, py: Python<'py>, sequence: u32) -> PyResult<Bound<'py, PyAny>> {
        output(py, self.0.action(sequence))
    }

    #[getter]
    fn business_error(&self) -> bool {
        self.0.business_error()
    }

    #[getter]
    fn human_reviewed(&self) -> Option<bool> {
        self.0.human_reviewed()
    }
}

fn validation_error(error: core::Error) -> PyErr {
    PyValueError::new_err(error.to_string())
}

/// Validate an R4 $submit Bundle and retain its response correlation data.
#[pyfunction]
fn validate_request(py: Python<'_>, document: String) -> PyResult<ValidatedRequest> {
    py.detach(move || {
        let value = decode(&document, "FHIR JSON")?;
        let request = core::validate_request(&value).map_err(validation_error)?;
        Ok(ValidatedRequest(request.into_owned()))
    })
}

#[pyfunction]
#[pyo3(signature = (document, request, *, additional_information_requested=None))]
fn validate_response(
    py: Python<'_>,
    document: String,
    request: &ValidatedRequest,
    additional_information_requested: Option<Vec<u32>>,
) -> PyResult<Response> {
    py.detach(move || {
        let value = decode(&document, "FHIR JSON")?;
        core::validate_response(
            &value,
            &request.0,
            core::ResponseContext {
                additional_information_requested: additional_information_requested
                    .as_deref()
                    .unwrap_or_default(),
            },
        )
        .map(Response)
        .map_err(validation_error)
    })
}

#[pyfunction]
fn validate_subscription(
    py: Python<'_>,
    document: String,
    sending_systems: Vec<String>,
) -> PyResult<()> {
    py.detach(move || {
        let value = decode(&document, "FHIR JSON")?;
        let sending_systems: Vec<_> = sending_systems.iter().map(String::as_str).collect();
        core::validate_subscription(&value, &sending_systems).map_err(validation_error)
    })
}

#[pyfunction]
fn validate_inquiry(inquiry: &Bound<'_, PyAny>) -> PyResult<()> {
    core::validate_inquiry(extract(inquiry, "PAS inquiry")?).map_err(validation_error)
}

/// Validate a notification; event notifications call validate_focus for each response JSON Bundle.
#[pyfunction]
#[pyo3(signature = (document, subscription_reference, validate_focus=None))]
fn validate_notification(
    py: Python<'_>,
    document: String,
    subscription_reference: String,
    validate_focus: Option<Py<PyAny>>,
) -> PyResult<()> {
    if validate_focus
        .as_ref()
        .is_some_and(|focus| !focus.bind(py).is_callable())
    {
        return Err(PyTypeError::new_err("validate_focus must be callable"));
    }
    py.detach(move || {
        let value = decode(&document, "FHIR JSON")?;
        let mut failure = None;
        let result = core::validate_notification(&value, &subscription_reference, |focus| {
            let Some(validate) = &validate_focus else {
                failure = Some(PyValueError::new_err(
                    "event notifications require validate_focus",
                ));
                return Err(core::Error::Notification);
            };
            let text = focus.to_string();
            Python::attach(|py| validate.call1(py, (text,)))
                .map(|_| ())
                .map_err(|error| {
                    failure = Some(error);
                    core::Error::Notification
                })
        });
        match failure {
            Some(error) => Err(error),
            None => result.map_err(validation_error),
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[pyclass(frozen, module = "medverify.fhir_pas")]
struct Timeline {
    payer: core::Payer,
    received: Zoned,
    priority: core::Priority,
    checked_at: Timestamp,
    extension: Option<core::Extension>,
    earlier_deadline: Option<Timestamp>,
}

impl Timeline {
    fn as_timeline(&self) -> core::Timeline<'_> {
        core::Timeline {
            payer: self.payer,
            received: &self.received,
            priority: self.priority,
            checked_at: self.checked_at,
            extension: self.extension,
            earlier_deadline: self.earlier_deadline,
        }
    }
}

fn timing_result(
    py: Python<'_>,
    result: Result<core::Timeliness, core::TimingError>,
) -> PyResult<Bound<'_, PyAny>> {
    output(
        py,
        result.map_err(|error| PyValueError::new_err(format!("{error:?}")))?,
    )
}

#[pymethods]
impl Timeline {
    #[new]
    fn new(data: &Bound<'_, PyAny>) -> PyResult<Self> {
        extract(data, "PAS timeline")
    }

    #[pyo3(signature = (kind, event=None))]
    fn assess<'py>(
        &self,
        py: Python<'py>,
        kind: &Bound<'_, PyAny>,
        event: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let kind = extract(kind, "PAS service kind")?;
        let event = event
            .map(|value| extract(value, "PAS communicated action"))
            .transpose()?;
        timing_result(py, self.as_timeline().assess(kind, event))
    }

    #[pyo3(signature = (request, sequence, *, classifications=None, event=None))]
    fn assess_item<'py>(
        &self,
        py: Python<'py>,
        request: &ValidatedRequest,
        sequence: u32,
        classifications: Option<&Bound<'_, PyAny>>,
        event: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let item = request
            .0
            .items()
            .find(|item| item.sequence == sequence)
            .ok_or_else(|| PyValueError::new_err("unknown request item sequence"))?;
        let classifications: Vec<(String, String, core::ServiceKind)> = classifications
            .map(|value| extract(value, "PAS service classifications"))
            .transpose()?
            .unwrap_or_default();
        let mut by_code = BTreeMap::new();
        for (system, code, kind) in &classifications {
            if by_code
                .insert((system.as_str(), code.as_str()), *kind)
                .is_some()
            {
                return Err(PyValueError::new_err(
                    "duplicate PAS service classification",
                ));
            }
        }
        let event = event
            .map(|value| extract(value, "PAS communicated action"))
            .transpose()?;
        timing_result(
            py,
            self.as_timeline().assess_item(
                item,
                |system, code| by_code.get(&(system, code)).copied(),
                event,
            ),
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewContext {
    state: String,
    plan: core::Plan,
    on: Date,
    issued_or_renewed: Option<Date>,
    decision: core::Decision,
    basis: core::Basis,
    specialty: core::Specialty,
}

impl ReviewContext {
    fn as_context(&self) -> core::ReviewContext<'_> {
        core::ReviewContext {
            state: &self.state,
            plan: self.plan,
            on: self.on,
            issued_or_renewed: self.issued_or_renewed,
            decision: self.decision,
            basis: self.basis,
            specialty: self.specialty,
        }
    }
}

#[pyclass(frozen, module = "medverify.fhir_pas")]
struct ReviewerPolicy(core::ReviewerPolicy);

#[pymethods]
impl ReviewerPolicy {
    #[staticmethod]
    fn state_rules() -> Self {
        Self(core::ReviewerPolicy::state_rules())
    }

    #[staticmethod]
    fn from_toml(text: &str) -> PyResult<Self> {
        core::ReviewerPolicy::from_toml(text)
            .map(Self)
            .map_err(|_| PyValueError::new_err("invalid PAS reviewer policy TOML"))
    }

    fn applicable_rules<'py>(
        &self,
        py: Python<'py>,
        context: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let context: ReviewContext = extract(context, "PAS review context")?;
        let rules = self
            .0
            .applicable_rules(context.as_context())
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        output(py, rules)
    }

    #[pyo3(signature = (context, automation, reviewer=None))]
    fn verify(
        &self,
        context: &Bound<'_, PyAny>,
        automation: &Bound<'_, PyAny>,
        reviewer: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let context: ReviewContext = extract(context, "PAS review context")?;
        let automation = extract(automation, "PAS automation")?;
        let reviewer = reviewer
            .map(|value| extract(value, "PAS reviewer evidence"))
            .transpose()?;
        self.0
            .verify(context.as_context(), automation, reviewer)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Timeline>()?;
    module.add_class::<ReviewerPolicy>()?;
    module.add_class::<ValidatedRequest>()?;
    module.add_class::<Response>()?;
    module.add_function(wrap_pyfunction!(validate_request, module)?)?;
    module.add_function(wrap_pyfunction!(validate_response, module)?)?;
    module.add_function(wrap_pyfunction!(validate_subscription, module)?)?;
    module.add_function(wrap_pyfunction!(validate_inquiry, module)?)?;
    module.add_function(wrap_pyfunction!(validate_notification, module)?)
}
