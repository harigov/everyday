//! FHIR resources (R4, and the DSTU2 an older portal sends) into a
//! [`RawDocument`]: a summary entry per resource, and a [`LabResult`] for
//! every laboratory `Observation` with a value, for the person to accept.
//!
//! The reader is tolerant by design. Portals and Apple Health spell the
//! same fact several ways (`effectiveDateTime` or `issued`, a `category`
//! that is one concept in DSTU2 and a list in R4, `medicationCodeableConcept`
//! or `medicationReference`), so each summary field is looked for under every
//! name it is known by and left empty when it is under none. What it will
//! not do is guess: an Observation with no date gets an entry but no lab
//! result, because a result filed under today's date would put a wrong point
//! on a trend.
//!
//! [`LabResult`]: super::super::LabResult

use super::super::{Code, DocKind, FhirEntry, LabFlag, LabOrigin, LabResult, RawDocument};
use crate::error::{Error, Result};
use crate::id::LabResultId;
use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::Value;

/// One file: a single resource or a `Bundle` of them.
///
/// The document's title is the one resource's own ("TSH 2.1 mIU/L") or, for
/// a bundle, how many it holds ("12 records"); its date the earliest the
/// resources carry. `external_id` is the resource's `id` (the bundle's, for
/// a bundle), or a hash of the file when it has none, so that importing the
/// same export twice finds the same document again.
pub fn read_file(json: &[u8], file_name: &str) -> Result<RawDocument> {
    let root: Value = serde_json::from_slice(json)
        .map_err(|_| Error::Invalid("that file is not valid JSON".into()))?;
    let resources = resources(&root)?;
    let now = Timestamp::now();
    let mut entries = Vec::with_capacity(resources.len());
    let mut labs = Vec::new();
    for resource in &resources {
        let entry = summarise(resource);
        labs.extend(lab_result(resource, &entry, now));
        entries.push(entry);
    }
    let title = match entries.as_slice() {
        [only] => title_of(only),
        many => format!("{} records", many.len()),
    };
    let id = text(&root, "id").map(str::to_string).unwrap_or_else(|| {
        // No id of its own (a hand-made file): what is in it is its identity.
        blake3::hash(json).to_hex().to_string()
    });
    Ok(RawDocument {
        title,
        kind: DocKind::Fhir,
        date: entries.iter().filter_map(|e| e.date).min(),
        provider: resources.iter().find_map(|r| provider_of(r)).unwrap_or_default(),
        file_name: file_name.to_string(),
        mime: "application/fhir+json".to_string(),
        bytes: json.to_vec(),
        fhir: entries,
        labs,
        external_id: id,
    })
}

/// A short, stable name for a code system: `loinc`, `snomed`, `rxnorm`,
/// `icd10`; any other system is kept as the URL it came as.
pub(crate) fn short_system(url: &str) -> String {
    let lower = url.trim().to_ascii_lowercase();
    let known = if lower.contains("loinc.org") || lower == "urn:oid:2.16.840.1.113883.6.1" {
        "loinc"
    } else if lower.contains("snomed.info") || lower == "urn:oid:2.16.840.1.113883.6.96" {
        "snomed"
    } else if lower.contains("rxnorm") || lower == "urn:oid:2.16.840.1.113883.6.88" {
        "rxnorm"
    } else if lower.contains("icd-10")
        || lower.contains("icd10")
        || lower == "urn:oid:2.16.840.1.113883.6.90"
    {
        "icd10"
    } else {
        return url.trim().to_string();
    };
    known.to_string()
}

/// The resources in a file: its entries if it is a `Bundle`, else itself.
fn resources(root: &Value) -> Result<Vec<&Value>> {
    let found: Vec<&Value> = match text(root, "resourceType") {
        Some("Bundle") => root
            .get("entry")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|e| e.get("resource"))
            .filter(|r| text(r, "resourceType").is_some())
            .collect(),
        Some(_) => vec![root],
        None => {
            return Err(Error::Invalid(
                "that file is not a FHIR resource: it has no resourceType".into(),
            ));
        }
    };
    if found.is_empty() {
        return Err(Error::Invalid("that FHIR bundle holds no resources".into()));
    }
    Ok(found)
}

// ---- reading values -------------------------------------------------------------

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Follow `path` through nested objects.
fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, key| v.get(*key))
}

/// The fields a resource's own date is in, most specific first. R4 and
/// DSTU2 between them.
const DATE_PATHS: &[&[&str]] = &[
    &["effectiveDateTime"],
    &["effectivePeriod", "start"],
    &["effectiveInstant"],
    &["issued"],
    &["onsetDateTime"],
    &["onsetPeriod", "start"],
    &["recordedDate"],
    &["dateRecorded"],
    &["authoredOn"],
    &["occurrenceDateTime"],
    &["dateWritten"],
    &["dateAsserted"],
    &["performedDateTime"],
    &["performedPeriod", "start"],
    &["date"],
    &["period", "start"],
];

/// The date part of a FHIR date or dateTime (`2023-11-01T08:30:00-07:00`,
/// `2023-11-01`, `2023-11`, `2023`), as written, in the zone it was written.
fn date_of(s: &str) -> Option<Date> {
    let s = s.trim();
    if let Some(day) = s.get(..10).and_then(|d| d.parse::<Date>().ok()) {
        return Some(day);
    }
    if let Some(month) = s.get(..7).and_then(|m| format!("{m}-01").parse::<Date>().ok()) {
        return Some(month);
    }
    s.get(..4).and_then(|y| format!("{y}-01-01").parse::<Date>().ok())
}

fn entry_date(resource: &Value) -> Option<Date> {
    DATE_PATHS.iter().find_map(|path| at(resource, path)?.as_str().and_then(date_of))
}

/// The fields that hold what a resource is *about*, as a CodeableConcept
/// (or a list of them, for which the first is taken).
const CONCEPT_FIELDS: &[&str] =
    &["code", "medicationCodeableConcept", "vaccineCode", "substance", "type"];

fn concept(resource: &Value) -> Option<&Value> {
    CONCEPT_FIELDS.iter().find_map(|field| {
        let v = resource.get(*field)?;
        if v.is_object() { Some(v) } else { v.as_array()?.first().filter(|c| c.is_object()) }
    })
}

/// What a person would call a CodeableConcept: its text, else the first
/// coding's display, else the first coding's code.
fn concept_text(concept: &Value) -> Option<String> {
    let codings = concept.get("coding").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    text(concept, "text")
        .or_else(|| codings.iter().find_map(|c| text(c, "display")))
        .or_else(|| codings.iter().find_map(|c| text(c, "code")))
        .map(str::to_string)
}

fn concept_codes(concept: &Value) -> Vec<Code> {
    let codings = concept.get("coding").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    codings
        .iter()
        .filter_map(|c| {
            Some(Code {
                system: short_system(text(c, "system").unwrap_or("")),
                code: text(c, "code")?.to_string(),
                display: text(c, "display").unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// `5.4 %`, `<0.5 ng/mL`: a Quantity as a person would write it.
fn quantity_text(q: &Value) -> Option<String> {
    let value = q.get("value")?.as_f64()?;
    let unit = text(q, "unit").or_else(|| text(q, "code")).unwrap_or("");
    let comparator = text(q, "comparator").unwrap_or("");
    Some(format!("{comparator}{value} {unit}").trim().to_string())
}

/// An Observation's value in words: a quantity, a string, a coded answer,
/// a yes or a no, or (a blood pressure panel) the values of its components.
fn observation_value(resource: &Value) -> String {
    if let Some(q) = resource.get("valueQuantity").and_then(quantity_text) {
        return q;
    }
    if let Some(s) = text(resource, "valueString") {
        return s.to_string();
    }
    if let Some(c) = resource.get("valueCodeableConcept").and_then(concept_text) {
        return c;
    }
    if let Some(b) = resource.get("valueBoolean").and_then(Value::as_bool) {
        return b.to_string();
    }
    if let Some(i) = resource.get("valueInteger").and_then(Value::as_i64) {
        return i.to_string();
    }
    let parts: Vec<String> = resource
        .get("component")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|c| c.get("valueQuantity").and_then(quantity_text))
        .collect();
    parts.join(", ")
}

/// The clinician, clinic or lab a resource names: a Reference's `display`,
/// from whichever of the fields that name one is present.
fn provider_of(resource: &Value) -> Option<String> {
    const FIELDS: &[&str] =
        &["performer", "requester", "recorder", "prescriber", "asserter", "author"];
    fn display(v: &Value) -> Option<String> {
        match v {
            Value::Array(items) => items.iter().find_map(display),
            // DSTU2's `requester` is `{agent: {display}}`.
            Value::Object(_) => {
                text(v, "display").or_else(|| text(v.get("agent")?, "display")).map(str::to_string)
            }
            _ => None,
        }
    }
    FIELDS.iter().find_map(|field| display(resource.get(*field)?))
}

// ---- entries ----------------------------------------------------------------------

/// One resource, summarised for the screen.
fn summarise(resource: &Value) -> FhirEntry {
    let resource_type = text(resource, "resourceType").unwrap_or("").to_string();
    let concept = concept(resource);
    let display = concept
        .and_then(concept_text)
        .or_else(|| text(resource.get("medicationReference")?, "display").map(str::to_string))
        .or_else(|| {
            ["description", "title", "name"]
                .iter()
                .find_map(|k| text(resource, k))
                .map(str::to_string)
        })
        .unwrap_or_default();
    FhirEntry {
        value: match resource_type.as_str() {
            "Observation" => observation_value(resource),
            _ => String::new(),
        },
        resource_type,
        display,
        date: entry_date(resource),
        codes: concept.map(concept_codes).unwrap_or_default(),
    }
}

/// "TSH 2.1 mIU/L" for an observation with a value, else what the resource
/// is called, else its type.
fn title_of(entry: &FhirEntry) -> String {
    match (entry.display.as_str(), entry.value.as_str()) {
        ("", "") => entry.resource_type.clone(),
        ("", value) => format!("{} {value}", entry.resource_type),
        (display, "") => display.to_string(),
        (display, value) => format!("{display} {value}"),
    }
}

// ---- lab results ------------------------------------------------------------------

/// Is this Observation categorised as laboratory? DSTU2 has one concept for
/// `category`, R4 a list.
fn is_laboratory(resource: &Value) -> bool {
    let categories: Vec<&Value> = match resource.get("category") {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(other) => vec![other],
        None => Vec::new(),
    };
    categories.iter().any(|category| {
        let codings =
            category.get("coding").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        codings
            .iter()
            .any(|k| text(k, "code").is_some_and(|code| code.eq_ignore_ascii_case("laboratory")))
    })
}

/// How an Observation's `interpretation` flags it. The HL7 codes come
/// first (`H`/`HH` high, `L`/`LL` low, `N` normal, `A`/`AA` abnormal, `CR`
/// critical); a portal that sent only words is read by those.
fn flag_of(resource: &Value) -> LabFlag {
    let concepts: Vec<&Value> = match resource.get("interpretation") {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(other) => vec![other],
        None => Vec::new(),
    };
    let codings = || {
        concepts
            .iter()
            .flat_map(|c| c.get("coding").and_then(Value::as_array).map_or(&[][..], Vec::as_slice))
    };
    for coding in codings() {
        let flag = match text(coding, "code").map(str::to_ascii_uppercase).as_deref() {
            Some("H" | "HH" | "HU" | ">") => LabFlag::High,
            Some("L" | "LL" | "LU" | "<") => LabFlag::Low,
            Some("N") => LabFlag::Normal,
            Some("A" | "AA") => LabFlag::Abnormal,
            Some("CR" | "CRH" | "CRL" | "CAR") => LabFlag::Critical,
            _ => continue,
        };
        return flag;
    }
    let words = concepts
        .iter()
        .filter_map(|c| text(c, "text"))
        .chain(codings().filter_map(|c| text(c, "display")))
        .map(str::to_ascii_lowercase);
    for word in words {
        // "abnormal" contains "normal": the longer words first.
        for (needle, flag) in [
            ("critical", LabFlag::Critical),
            ("abnormal", LabFlag::Abnormal),
            ("high", LabFlag::High),
            ("low", LabFlag::Low),
            ("normal", LabFlag::Normal),
        ] {
            if word.contains(needle) {
                return flag;
            }
        }
    }
    LabFlag::None
}

/// The lab result an Observation holds, if it holds one: a numeric
/// `valueQuantity` on an Observation that is categorised as laboratory or
/// carries a LOINC code, that has a date and a name, and was not withdrawn.
/// It is a proposal: the person accepts it, or not, beside the document.
fn lab_result(resource: &Value, entry: &FhirEntry, now: Timestamp) -> Option<LabResult> {
    let withdrawn = text(resource, "status") == Some("entered-in-error");
    if entry.resource_type != "Observation" || withdrawn {
        return None;
    }
    let quantity = resource.get("valueQuantity")?;
    let value = quantity.get("value")?.as_f64()?;
    let loinc = entry.codes.iter().find(|c| c.system == "loinc");
    if loinc.is_none() && !is_laboratory(resource) {
        return None;
    }
    let collected = entry.date?;
    let name = Some(entry.display.as_str())
        .filter(|d| !d.is_empty())
        .or(loinc.map(|c| c.code.as_str()))?
        .to_string();
    let range = resource.get("referenceRange").and_then(Value::as_array).and_then(|r| r.first());
    let bound = |side: &str| range?.get(side)?.get("value")?.as_f64();
    // An instant only when the resource says one; a bare date is not one.
    let collected_at = ["effectiveDateTime", "effectiveInstant"]
        .iter()
        .find_map(|k| text(resource, k))
        .and_then(|s| s.parse::<Timestamp>().ok());
    Some(LabResult {
        id: LabResultId::new(),
        code: loinc.map(|c| c.code.clone()).unwrap_or_default(),
        name,
        value: Some(value),
        text: String::new(),
        unit: text(quantity, "unit").or_else(|| text(quantity, "code")).unwrap_or("").to_string(),
        ref_low: bound("low"),
        ref_high: bound("high"),
        ref_text: range.and_then(|r| text(r, "text")).unwrap_or("").to_string(),
        flag: flag_of(resource),
        collected,
        collected_at,
        lab: provider_of(resource).unwrap_or_default(),
        panel: String::new(),
        document_id: None,
        origin: LabOrigin::Fhir,
        note: String::new(),
        created_at: now,
        updated_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSH: &str = r#"{
        "resourceType": "Observation",
        "id": "obs-tsh-1",
        "status": "final",
        "category": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/observation-category", "code": "laboratory"}]}],
        "code": {"coding": [{"system": "http://loinc.org", "code": "3016-3", "display": "Thyrotropin [Units/volume] in Serum or Plasma"}], "text": "TSH"},
        "effectiveDateTime": "2023-10-12T08:30:00-07:00",
        "valueQuantity": {"value": 2.1, "unit": "mIU/L", "system": "http://unitsofmeasure.org", "code": "mIU/L"},
        "referenceRange": [{"low": {"value": 0.4, "unit": "mIU/L"}, "high": {"value": 4.0, "unit": "mIU/L"}, "text": "0.4-4.0 mIU/L"}],
        "interpretation": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/v3-ObservationInterpretation", "code": "N"}]}],
        "performer": [{"display": "Quest Diagnostics"}]
    }"#;

    const DIABETES: &str = r#"{
        "resourceType": "Condition",
        "id": "cond-1",
        "code": {"coding": [{"system": "http://snomed.info/sct", "code": "44054006", "display": "Type 2 diabetes mellitus"}]},
        "onsetDateTime": "2018-03-02",
        "recordedDate": "2018-03-05T00:00:00Z",
        "recorder": {"display": "Dr. A. Rivera"}
    }"#;

    #[test]
    fn a_lab_observation_is_an_entry_and_a_lab_result() {
        let doc = read_file(TSH.as_bytes(), "Observation-1.json").unwrap();
        assert_eq!(doc.title, "TSH 2.1 mIU/L");
        assert_eq!(doc.kind, DocKind::Fhir);
        assert_eq!(doc.date, Some("2023-10-12".parse().unwrap()));
        assert_eq!(doc.provider, "Quest Diagnostics");
        assert_eq!(doc.file_name, "Observation-1.json");
        assert_eq!(doc.mime, "application/fhir+json");
        assert_eq!(doc.bytes, TSH.as_bytes());
        assert_eq!(doc.external_id, "obs-tsh-1");

        assert_eq!(doc.fhir.len(), 1);
        let entry = &doc.fhir[0];
        assert_eq!(entry.resource_type, "Observation");
        assert_eq!(entry.display, "TSH");
        assert_eq!(entry.value, "2.1 mIU/L");
        assert_eq!(
            entry.codes,
            vec![Code {
                system: "loinc".into(),
                code: "3016-3".into(),
                display: "Thyrotropin [Units/volume] in Serum or Plasma".into(),
            }]
        );

        assert_eq!(doc.labs.len(), 1);
        let lab = &doc.labs[0];
        assert_eq!((lab.code.as_str(), lab.name.as_str()), ("3016-3", "TSH"));
        assert_eq!((lab.value, lab.unit.as_str()), (Some(2.1), "mIU/L"));
        assert_eq!((lab.ref_low, lab.ref_high), (Some(0.4), Some(4.0)));
        assert_eq!(lab.ref_text, "0.4-4.0 mIU/L");
        assert_eq!(lab.flag, LabFlag::Normal);
        assert_eq!(lab.collected, "2023-10-12".parse::<Date>().unwrap());
        assert_eq!(lab.collected_at, Some("2023-10-12T15:30:00Z".parse().unwrap()));
        assert_eq!(lab.lab, "Quest Diagnostics");
        assert_eq!(lab.origin, LabOrigin::Fhir);
        assert_eq!(lab.document_id, None);
    }

    #[test]
    fn a_condition_is_an_entry_and_no_lab() {
        let doc = read_file(DIABETES.as_bytes(), "Condition-1.json").unwrap();
        assert_eq!(doc.title, "Type 2 diabetes mellitus");
        assert_eq!(doc.date, Some("2018-03-02".parse().unwrap()));
        assert_eq!(doc.provider, "Dr. A. Rivera");
        assert!(doc.labs.is_empty());
        let entry = &doc.fhir[0];
        assert_eq!(entry.value, "");
        assert_eq!(entry.codes.len(), 1);
        let code = &entry.codes[0];
        assert_eq!((code.system.as_str(), code.code.as_str()), ("snomed", "44054006"));
    }

    #[test]
    fn a_bundle_is_one_document_with_every_resource() {
        let bundle = format!(
            r#"{{"resourceType": "Bundle", "id": "bundle-9", "type": "collection",
                "entry": [{{"resource": {TSH}}}, {{"fullUrl": "urn:x"}}, {{"resource": {DIABETES}}}]}}"#
        );
        let doc = read_file(bundle.as_bytes(), "export.json").unwrap();
        assert_eq!(doc.title, "2 records");
        assert_eq!(doc.fhir.len(), 2);
        assert_eq!(doc.labs.len(), 1);
        assert_eq!(doc.date, Some("2018-03-02".parse().unwrap()), "the earliest of the two");
        assert_eq!(doc.external_id, "bundle-9");
        assert_eq!(doc.provider, "Quest Diagnostics", "the first resource that names one");
    }

    #[test]
    fn a_file_with_no_id_is_known_by_its_hash() {
        let json = r#"{"resourceType": "Condition", "code": {"text": "Asthma"}}"#;
        let a = read_file(json.as_bytes(), "a.json").unwrap();
        let b = read_file(json.as_bytes(), "b.json").unwrap();
        assert_eq!(a.external_id.len(), 64);
        assert_eq!(a.external_id, b.external_id);
        let gout = br#"{"resourceType": "Condition", "code": {"text": "Gout"}}"#;
        assert_ne!(a.external_id, read_file(gout, "c.json").unwrap().external_id);
    }

    #[test]
    fn dstu2_spellings_are_read() {
        let order = r#"{
            "resourceType": "MedicationOrder",
            "id": "mo-1",
            "dateWritten": "2023-05-01",
            "prescriber": {"display": "Dr. B. Okafor"},
            "medicationCodeableConcept": {
                "coding": [{"system": "http://www.nlm.nih.gov/research/umls/rxnorm", "code": "966247", "display": "Levothyroxine 50 MCG Oral Tablet"}],
                "text": "Levothyroxine 50 mcg"
            }
        }"#;
        let doc = read_file(order.as_bytes(), "MedicationOrder-1.json").unwrap();
        assert_eq!(doc.title, "Levothyroxine 50 mcg");
        assert_eq!(doc.date, Some("2023-05-01".parse().unwrap()));
        assert_eq!(doc.provider, "Dr. B. Okafor");
        assert_eq!(doc.fhir[0].codes[0].system, "rxnorm");

        // DSTU2 has one `category` concept where R4 has a list, and one
        // `interpretation` concept where R4 has a list.
        let obs = r#"{
            "resourceType": "Observation",
            "category": {"coding": [{"system": "http://hl7.org/fhir/observation-category", "code": "laboratory"}]},
            "code": {"text": "Hemoglobin A1c"},
            "effectiveDateTime": "2023-09-01",
            "valueQuantity": {"value": 7.4, "unit": "%"},
            "interpretation": {"coding": [{"code": "H"}]}
        }"#;
        let doc = read_file(obs.as_bytes(), "Observation-2.json").unwrap();
        assert_eq!(doc.title, "Hemoglobin A1c 7.4 %");
        assert_eq!(doc.labs.len(), 1);
        assert_eq!(doc.labs[0].flag, LabFlag::High);
        assert_eq!(doc.labs[0].code, "", "no LOINC code on it");
        assert_eq!(doc.labs[0].collected_at, None, "a date alone is not an instant");
    }

    #[test]
    fn an_observation_that_is_not_a_lab_result_is_only_an_entry() {
        let no_date = r#"{"resourceType": "Observation", "category": [{"coding": [{"code": "laboratory"}]}],
            "code": {"text": "Glucose"}, "valueQuantity": {"value": 5.5, "unit": "mmol/L"}}"#;
        let doc = read_file(no_date.as_bytes(), "a.json").unwrap();
        assert!(doc.labs.is_empty(), "no date, no lab result");
        assert_eq!(doc.fhir[0].value, "5.5 mmol/L");
        assert_eq!(doc.date, None);

        let survey = r#"{"resourceType": "Observation", "category": [{"coding": [{"code": "survey"}]}],
            "code": {"text": "Smoking status"}, "effectiveDateTime": "2023-01-01", "valueQuantity": {"value": 1}}"#;
        let doc = read_file(survey.as_bytes(), "b.json").unwrap();
        assert!(doc.labs.is_empty(), "not laboratory, no LOINC");

        let text_result = r#"{"resourceType": "Observation", "category": [{"coding": [{"code": "laboratory"}]}],
            "code": {"text": "Urine culture"}, "effectiveDateTime": "2023-01-01", "valueString": "No growth"}"#;
        let doc = read_file(text_result.as_bytes(), "c.json").unwrap();
        assert!(doc.labs.is_empty());
        assert_eq!(doc.title, "Urine culture No growth");

        let withdrawn = TSH.replace("\"final\"", "\"entered-in-error\"");
        assert!(read_file(withdrawn.as_bytes(), "d.json").unwrap().labs.is_empty());
    }

    #[test]
    fn a_blood_pressure_panel_shows_its_components() {
        let json = r#"{"resourceType": "Observation", "code": {"text": "Blood pressure"},
            "effectiveDateTime": "2023-01-01T09:00:00Z",
            "component": [{"code": {"text": "Systolic"}, "valueQuantity": {"value": 128, "unit": "mmHg"}},
                          {"code": {"text": "Diastolic"}, "valueQuantity": {"value": 82, "unit": "mmHg"}}]}"#;
        let doc = read_file(json.as_bytes(), "bp.json").unwrap();
        assert_eq!(doc.fhir[0].value, "128 mmHg, 82 mmHg");
        assert!(doc.labs.is_empty());
    }

    #[test]
    fn interpretations_map_to_flags() {
        let flag = |code: &str| {
            flag_of(&serde_json::json!({"interpretation": [{"coding": [{"code": code}]}]}))
        };
        assert_eq!(flag("H"), LabFlag::High);
        assert_eq!(flag("HH"), LabFlag::High);
        assert_eq!(flag("L"), LabFlag::Low);
        assert_eq!(flag("LL"), LabFlag::Low);
        assert_eq!(flag("N"), LabFlag::Normal);
        assert_eq!(flag("A"), LabFlag::Abnormal);
        assert_eq!(flag("AA"), LabFlag::Abnormal);
        assert_eq!(flag("CR"), LabFlag::Critical);
        assert_eq!(flag("??"), LabFlag::None);
        let words = |t: &str| flag_of(&serde_json::json!({"interpretation": [{"text": t}]}));
        assert_eq!(words("Abnormal"), LabFlag::Abnormal);
        assert_eq!(words("Critical high"), LabFlag::Critical);
        assert_eq!(words("Normal"), LabFlag::Normal);
        assert_eq!(flag_of(&serde_json::json!({})), LabFlag::None);
    }

    #[test]
    fn code_systems_get_short_names() {
        assert_eq!(short_system("http://loinc.org"), "loinc");
        assert_eq!(short_system("http://snomed.info/sct"), "snomed");
        assert_eq!(short_system("http://www.nlm.nih.gov/research/umls/rxnorm"), "rxnorm");
        assert_eq!(short_system("http://hl7.org/fhir/sid/icd-10-cm"), "icd10");
        assert_eq!(short_system("http://hl7.org/fhir/sid/icd-10"), "icd10");
        assert_eq!(short_system("http://www.cms.gov/Medicare/Coding/ICD10"), "icd10");
        assert_eq!(short_system("http://hl7.org/fhir/sid/ndc"), "http://hl7.org/fhir/sid/ndc");
        assert_eq!(short_system(""), "");
    }

    #[test]
    fn dates_are_cut_to_the_day() {
        let d = |s: &str| date_of(s).map(|d| d.to_string());
        assert_eq!(d("2023-11-01T08:30:00-07:00").as_deref(), Some("2023-11-01"));
        assert_eq!(d("2023-11-01").as_deref(), Some("2023-11-01"));
        assert_eq!(d("2023-11").as_deref(), Some("2023-11-01"));
        assert_eq!(d("2023").as_deref(), Some("2023-01-01"));
        assert_eq!(d("last week"), None);
    }

    #[test]
    fn what_is_not_fhir_is_refused() {
        for bad in [
            "",
            "not json",
            "[1, 2]",
            r#"{"foo": 1}"#,
            r#"{"resourceType": "Bundle", "entry": []}"#,
        ] {
            let read = read_file(bad.as_bytes(), "x.json");
            assert!(matches!(read, Err(Error::Invalid(_))), "{bad:?}");
        }
    }
}
