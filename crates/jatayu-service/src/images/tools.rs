//! Making and drawing pictures, as the assistant's tools.
//!
//! - **`generate_image`** asks the image model in Settings → Models for a
//!   picture from a description ([`Service::generate_image`]). For what a
//!   model paints well: a scene, a portrait, a style.
//! - **`draw_illustration`** draws an SVG or an HTML page the assistant
//!   writes ([`crate::illustrate`]). For what has to be exact: a diagram, a
//!   chart, a timeline, anything with words in it, which an image model
//!   garbles.
//!
//! Either keeps what it made in Files ([`super::keep`]) and answers its id as
//! `file_id`, which the turn reads back out ([`files_of`]) and carries on the
//! call's card and its stored result -- that is how the panel draws the
//! picture with the reply, and how a chat sends it, without the model
//! having to link it.
//!
//! # How many
//!
//! Each turn has an [`Allowance`]: a few pictures from the model, which cost
//! money or minutes each, and more drawings, which cost neither. A model
//! that loops is stopped there rather than at the person's bill.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use jatayu_core::files::{FileEntry, FileOrigin};
use jatayu_core::id::FileId;
use rig_agent::core::tool::{PortableDynamicTool, ToolExecutionError, ToolOutput};
use serde_json::{Value, json};

use super::{Encoding, Shape, keep};
use crate::error::{CommandError, codes};
use crate::illustrate::{self, Markup};
use crate::service::Service;

pub const GENERATE_IMAGE: &str = "generate_image";
pub const DRAW_ILLUSTRATION: &str = "draw_illustration";

/// The most pictures one turn may ask the image model for.
pub const GENERATED_PER_TURN: u32 = 4;

/// The most drawings one turn may make.
pub const DRAWN_PER_TURN: u32 = 8;

/// Whether `name` is one of these tools.
pub fn is_tool(name: &str) -> bool {
    matches!(name, GENERATE_IMAGE | DRAW_ILLUSTRATION)
}

/// What one turn may still make. Shared by both tools of that turn.
pub struct Allowance {
    generated: AtomicU32,
    drawn: AtomicU32,
}

impl Allowance {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            generated: AtomicU32::new(GENERATED_PER_TURN),
            drawn: AtomicU32::new(DRAWN_PER_TURN),
        })
    }
}

/// The files a successful call of one of these tools made, read from its own
/// result. Empty for any other tool, whatever its result says: only these
/// two write `file_id`, and only what they made is drawn as theirs.
pub fn files_of(tool: &str, result: &Value) -> Vec<FileId> {
    if !is_tool(tool) {
        return Vec::new();
    }
    result
        .get("file_id")
        .and_then(Value::as_str)
        .and_then(|id| id.parse().ok())
        .into_iter()
        .collect()
}

/// The pictures a conversation's calls of these tools made, in the order they
/// were made, read back from its stored results: what a routine's run made,
/// for its delivery to send with its result. A call that failed made none;
/// a picture trashed since is still named, for whoever sends it to skip.
pub fn made_in(messages: &[jatayu_core::agent::Message]) -> Vec<FileId> {
    use jatayu_core::agent::Role;
    let called: std::collections::HashMap<&str, &str> = messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.tool_calls.iter().map(|c| (c.id.as_str(), c.name.as_str())))
        .collect();
    messages
        .iter()
        .filter(|m| m.role == Role::Tool && !m.failed)
        .filter(|m| {
            m.tool_call_id.as_deref().and_then(|id| called.get(id)).is_some_and(|n| is_tool(n))
        })
        .flat_map(|m| m.files.iter().copied())
        .collect()
}

/// The card's sentence: what the picture was called.
pub fn summarise(result: &Value) -> String {
    result.get("name").and_then(Value::as_str).unwrap_or_default().to_string()
}

/// `generate_image`. `local` says whether the chosen model runs on this
/// computer, which the model is told because it decides how many are worth
/// waiting for.
pub fn generate_image_tool(
    svc: Arc<Service>,
    origin: FileOrigin,
    allowance: Arc<Allowance>,
    local: bool,
) -> PortableDynamicTool {
    let speed = if local {
        "Their image model runs on this computer and takes a minute or more for each picture, \
         so make one and ask before making more."
    } else {
        "Each picture takes several seconds and costs them a little, so make one unless they \
         ask for more."
    };
    PortableDynamicTool::new(
        GENERATE_IMAGE,
        format!(
            "Make a picture with their image model from a written description: a scene, a \
             portrait, an object, a mood, a painting or photo style. The image model sees only \
             your description -- not this conversation, not earlier pictures, not who anyone \
             is -- so write it complete: lead with the subject, then the setting, composition, \
             style or medium, light and colour, in one paragraph of plain sentences. Describe \
             people, pets and places by how they look, since a name means nothing to it. To \
             change a picture you made, describe the whole picture again with the change. Keep \
             everything they asked for exactly, and fill in only what they left open. For a \
             diagram, chart, map, timeline, labelled figure or anything whose words or exact \
             shapes matter, use draw_illustration instead. The picture is saved to their Files \
             and shown with your reply, so do not link it or describe it at length. {speed}"
        ),
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The complete description the image model paints from, \
                                    subject first. It sees nothing else.",
                },
                "title": {
                    "type": "string",
                    "description": "A short name for the picture, a few words: what its \
                                    file is called.",
                },
                "shape": {
                    "type": "string",
                    "enum": ["square", "wide", "tall"],
                    "description": "Square (the default), wide (3:2 landscape) or tall \
                                    (2:3 portrait).",
                },
            },
            "required": ["prompt", "title"],
            "additionalProperties": false,
        }),
        move |arguments: Value| {
            let (svc, allowance) = (svc.clone(), allowance.clone());
            Box::pin(async move {
                let prompt = text(&arguments, "prompt")
                    .ok_or_else(|| invalid("generate_image: `prompt` is required"))?;
                let title = text(&arguments, "title").unwrap_or("Picture");
                let shape = match text(&arguments, "shape") {
                    None => Shape::Square,
                    Some(s) => Shape::parse(s).ok_or_else(|| {
                        invalid("generate_image: `shape` is square, wide or tall")
                    })?,
                };
                if !crate::agent::take_one(&allowance.generated) {
                    return Err(ToolExecutionError::other(format!(
                        "That is {GENERATED_PER_TURN} pictures this turn, the most one turn may \
                         make. Show them these and ask before making more."
                    )));
                }
                let made_one = async {
                    let png = svc.generate_image(prompt, shape).await?;
                    let vault = svc.require_unlocked()?;
                    keep(&svc, &vault, png, Encoding::Smallest, title, prompt, origin).await
                };
                // Only a picture made counts against the turn: a busy
                // painter or a provider's bad minute is not one.
                let entry = made_one.await.map_err(|e| {
                    give_back(&allowance.generated);
                    tool_error(e)
                })?;
                Ok(ToolOutput::json(made(&entry, &[])))
            })
        },
    )
}

/// `draw_illustration`. `html` says whether an HTML page can be drawn here;
/// without it the tool takes SVG alone, and says so.
pub fn draw_illustration_tool(
    svc: Arc<Service>,
    origin: FileOrigin,
    allowance: Arc<Allowance>,
    html: bool,
) -> PortableDynamicTool {
    let (formats, how): (&[&str], &str) = if html {
        (
            &["svg", "html"],
            "Prefer SVG: it is drawn at once and is shorter to write. Use HTML for a layout \
             that is mostly text -- wrapping paragraphs, a table, cards in a flex or grid \
             layout; it is laid out `width` CSS pixels wide (800 by default), as tall as its \
             content, drawn at twice that for sharp text, with the browser's default 8px body \
             margin unless you style it.",
        )
    } else {
        (&["svg"], "Only SVG can be drawn on this computer.")
    };
    PortableDynamicTool::new(
        DRAW_ILLUSTRATION,
        format!(
            "Draw something that explains visually -- a diagram, a flowchart, a timeline, a \
             chart, a comparison, a labelled sketch, a card, a poster -- by writing it as \
             markup, which is drawn into a picture, saved to their Files and shown to them \
             with your reply (so do not link it). {how} Everything has to be inside the \
             markup: scripts do not run, and nothing is fetched -- no web fonts, stylesheets \
             or pictures from addresses; a picture can only be a data: URL. Give an SVG a \
             viewBox and a width and height. Use generic font families (sans-serif, serif, \
             monospace). The paper is white unless you paint a background. Every number, \
             date and label in a chart or timeline comes from this conversation or your \
             tools; never make up data to fill one. SVG text does not wrap: break long labels \
             into lines with <tspan>, allow about 0.6 times the font size per character, and \
             leave margins. Keep it clean: few colours, text no smaller than 14px at the \
             drawing's size, room around everything. You cannot see the result: if the \
             answer says something was cut off, draw it again to fit."
        ),
        json!({
            "type": "object",
            "properties": {
                "format": {
                    "type": "string",
                    "enum": formats,
                    "description": "What `source` is written in.",
                },
                "source": {
                    "type": "string",
                    "description": "The whole SVG document, or the HTML to put in a page \
                                    (a fragment with a <style>, or a whole document).",
                },
                "title": {
                    "type": "string",
                    "description": "A short name for the picture, a few words: what its \
                                    file is called.",
                },
                "description": {
                    "type": "string",
                    "description": "One sentence saying what it shows, for someone who \
                                    cannot see it. Kept as its caption.",
                },
                "width": {
                    "type": "integer",
                    "description": "For SVG, how many pixels wide the picture is (its own \
                                    width when left out). For HTML, the page's width in CSS \
                                    pixels (800 when left out).",
                },
                "height": {
                    "type": "integer",
                    "description": "HTML only: the page's height in CSS pixels. Leave it out \
                                    to fit the content.",
                },
            },
            "required": ["format", "source", "title"],
            "additionalProperties": false,
        }),
        move |arguments: Value| {
            let (svc, allowance) = (svc.clone(), allowance.clone());
            Box::pin(async move {
                let markup = text(&arguments, "format")
                    .and_then(Markup::parse)
                    .ok_or_else(|| invalid("draw_illustration: `format` is svg or html"))?;
                let source = text(&arguments, "source")
                    .ok_or_else(|| invalid("draw_illustration: `source` is required"))?;
                let title = text(&arguments, "title").unwrap_or("Illustration");
                let caption = text(&arguments, "description").unwrap_or_default();
                let size = |key: &str| {
                    arguments
                        .get(key)
                        .and_then(Value::as_u64)
                        .map(|n| n.min(u64::from(u32::MAX)) as u32)
                };
                if !crate::agent::take_one(&allowance.drawn) {
                    return Err(ToolExecutionError::other(format!(
                        "That is {DRAWN_PER_TURN} drawings this turn, the most one turn may \
                         make. Show them these first."
                    )));
                }
                let drawn_one = async {
                    let drawn =
                        illustrate::render(markup, source, size("width"), size("height")).await?;
                    let vault = svc.require_unlocked()?;
                    let entry =
                        keep(&svc, &vault, drawn.png, Encoding::AsMade, title, caption, origin)
                            .await?;
                    Ok::<_, CommandError>((entry, drawn.notes))
                };
                // Only a drawing made counts: markup that would not draw is
                // to be written again, and the retry must not be refused.
                let (entry, notes) = drawn_one.await.map_err(|e| {
                    give_back(&allowance.drawn);
                    tool_error(e)
                })?;
                Ok(ToolOutput::json(made(&entry, &notes)))
            })
        },
    )
}

/// Put back what a failed call took from the turn's allowance. A model that
/// fails for ever is stopped by the turn's own step limit, not by this.
fn give_back(budget: &AtomicU32) {
    budget.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// What either tool answers: the file, how big the picture is, and anything
/// the drawing had to say about itself.
fn made(entry: &FileEntry, notes: &[String]) -> Value {
    let mut out = json!({
        "file_id": entry.id.to_string(),
        "name": entry.name,
        "link": format!("files:{}", entry.id),
        "shown": true,
    });
    if let (Some(width), Some(height)) = (entry.media.width, entry.media.height) {
        out["size"] = json!(format!("{width}\u{d7}{height}"));
    }
    if !notes.is_empty() {
        out["notes"] = json!(notes);
    }
    out
}

fn text<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn invalid(message: &str) -> ToolExecutionError {
    ToolExecutionError::invalid_args(message.to_string())
}

/// A failure as the model is told it: something to change in what it asked
/// for, or something that went wrong on the way.
fn tool_error(e: CommandError) -> ToolExecutionError {
    if e.code == codes::INVALID || e.code == codes::TOO_LARGE {
        ToolExecutionError::invalid_args(e.message)
    } else {
        ToolExecutionError::other(e.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_these_tools_hand_over_the_files_they_made() {
        let id = FileId::new();
        let result = json!({ "file_id": id.to_string(), "name": "Fox.jpg" });
        assert_eq!(files_of(GENERATE_IMAGE, &result), vec![id]);
        assert_eq!(files_of(DRAW_ILLUSTRATION, &result), vec![id]);
        // Another tool's result that happens to say `file_id` is not a
        // picture to draw with the reply.
        assert!(files_of("find_files", &result).is_empty());
        assert!(files_of(GENERATE_IMAGE, &json!({ "file_id": "nonsense" })).is_empty());
        assert_eq!(summarise(&result), "Fox.jpg");
    }

    #[test]
    fn a_run_s_pictures_are_read_back_from_its_own_calls() {
        use jatayu_core::agent::{Message, Role, ToolCall};
        let conversation = jatayu_core::ConversationId::new();
        let call = |id: &str, name: &str| ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({}),
        };
        let asked =
            Message::new(conversation, Role::Assistant, String::new()).with_tool_calls(vec![
                call("a", DRAW_ILLUSTRATION),
                call("b", GENERATE_IMAGE),
                call("c", "find_files"),
            ]);
        let result = |id: &str, ok: bool, files: Vec<FileId>| {
            let mut m = Message::tool_result(
                conversation,
                &call(id, ""),
                if ok { Ok(String::new()) } else { Err(String::new()) },
                None,
            );
            m.files = files;
            m
        };
        let (drawn, painted, found) = (FileId::new(), FileId::new(), FileId::new());
        let messages = vec![
            asked,
            result("a", true, vec![drawn]),
            result("b", false, vec![painted]),
            // Another tool's result is not a picture this run made.
            result("c", true, vec![found]),
        ];
        assert_eq!(made_in(&messages), vec![drawn]);
    }

    #[test]
    fn a_turn_runs_out_of_pictures_before_drawings() {
        let allowance = Allowance::new();
        for _ in 0..GENERATED_PER_TURN {
            assert!(crate::agent::take_one(&allowance.generated));
        }
        assert!(!crate::agent::take_one(&allowance.generated));
        assert!(crate::agent::take_one(&allowance.drawn));
        // A call that failed gives its turn back.
        give_back(&allowance.generated);
        assert!(crate::agent::take_one(&allowance.generated));
        assert!(!crate::agent::take_one(&allowance.generated));
    }

    /// Markup that will not draw, written again and again, is told why each
    /// time rather than told the turn has used up its drawings.
    #[tokio::test]
    async fn a_drawing_that_failed_does_not_count_against_the_turn() {
        let svc = Arc::new(Service::new());
        let allowance = Allowance::new();
        let tool = draw_illustration_tool(svc, FileOrigin::Assistant, allowance.clone(), false);
        for _ in 0..DRAWN_PER_TURN + 2 {
            let err = tool
                .execute(json!({ "format": "svg", "source": "<p>no</p>", "title": "x" }))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("not an SVG"), "{err}");
        }
        assert_eq!(allowance.drawn.load(std::sync::atomic::Ordering::SeqCst), DRAWN_PER_TURN);
    }

    #[test]
    fn the_picture_tool_says_the_image_model_is_stateless_and_to_lead_with_the_subject() {
        let svc = Arc::new(Service::new());
        let tool = |local: bool| {
            generate_image_tool(svc.clone(), FileOrigin::Assistant, Allowance::new(), local)
                .definition()
        };
        let definition = tool(false);
        for must in [
            "sees only your description",
            "not this conversation, not earlier pictures",
            "lead with the subject",
            "Describe people, pets and places by how they look",
            "describe the whole picture again with the change",
            "Keep everything they asked for exactly",
            "use draw_illustration instead",
            "Each picture takes several seconds",
        ] {
            assert!(definition.description.contains(must), "{must}: {}", definition.description);
        }
        assert!(tool(true).description.contains("takes a minute or more"));
        let prompt = definition.parameters["properties"]["prompt"]["description"].as_str().unwrap();
        assert!(
            prompt.contains("subject first") && prompt.contains("sees nothing else"),
            "{prompt}"
        );
    }

    #[test]
    fn the_drawing_tool_forbids_made_up_data_and_says_svg_text_does_not_wrap() {
        let svc = Arc::new(Service::new());
        let description =
            draw_illustration_tool(svc, FileOrigin::Assistant, Allowance::new(), true)
                .definition()
                .description;
        for must in [
            "comes from this conversation or your tools",
            "never make up data",
            "SVG text does not wrap",
            "<tspan>",
            "0.6 times the font size per character",
            "You cannot see the result",
        ] {
            assert!(description.contains(must), "{must}: {description}");
        }
    }

    #[test]
    fn without_a_browser_the_drawing_tool_takes_svg_alone() {
        let svc = Arc::new(Service::new());
        let formats = |html: bool| {
            let tool =
                draw_illustration_tool(svc.clone(), FileOrigin::Assistant, Allowance::new(), html);
            tool.definition().parameters["properties"]["format"]["enum"].clone()
        };
        assert_eq!(formats(false), json!(["svg"]));
        assert_eq!(formats(true), json!(["svg", "html"]));
    }
}
