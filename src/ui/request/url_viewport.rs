//! A horizontal viewport for the URL's single-line text editor.

use std::time::Duration;

use iced::advanced::text::{self, Paragraph, paragraph};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer};
use iced::widget::text_editor;
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, alignment, window};
use unicode_segmentation::UnicodeSegmentation;

use crate::message::Message;
use crate::ui::theme::{MONO, TEXT_MD};

const CARET_MARGIN: f32 = 6.0;
const DRAG_EDGE: f32 = 16.0;
const DRAG_INTERVAL: Duration = Duration::from_millis(16);

pub(super) fn url_viewport<'a>(
    editor: impl Into<Element<'a, Message>>,
    tab: &'a str,
    content: &'a text_editor::Content,
) -> Element<'a, Message> {
    Element::new(UrlViewport {
        editor: editor.into(),
        tab,
        content,
    })
}

struct UrlViewport<'a> {
    editor: Element<'a, Message>,
    tab: &'a str,
    content: &'a text_editor::Content,
}

#[derive(Default)]
struct State {
    tab: String,
    paragraph: paragraph::Plain<<iced::Renderer as text::Renderer>::Paragraph>,
    offset: f32,
    last_caret: f32,
    last_width: f32,
    dragging: bool,
    last_drag_tick: Option<iced::time::Instant>,
}

impl Widget<Message, iced::Theme, iced::Renderer> for UrlViewport<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            tab: self.tab.to_owned(),
            ..State::default()
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.editor)]
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if state.tab != self.tab {
            self.tab.clone_into(&mut state.tab);
            state.offset = 0.0;
            state.last_width = 0.0;
            state.dragging = false;
            state.last_drag_tick = None;
        }
        tree.diff_children(std::slice::from_ref(&self.editor));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let line = self.content.line(0);
        let url = line.as_ref().map_or("", |line| line.text.as_ref());
        let reshaped = state.paragraph.update(text::Text {
            content: url,
            bounds: Size::INFINITE,
            size: TEXT_MD.into(),
            line_height: text::LineHeight::default(),
            font: MONO,
            align_x: text::Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
        });
        let width = limits.max().width;
        let child_width = width.max(state.paragraph.min_width().ceil() + CARET_MARGIN);
        let column = self.content.cursor().position.column.min(url.len());
        let grapheme = url[..url.floor_char_boundary(column)]
            .graphemes(false)
            .count();
        let caret = if column == url.len() {
            state.paragraph.min_width()
        } else {
            state
                .paragraph
                .raw()
                .grapheme_position(0, grapheme)
                .map_or(0.0, |point| point.x)
        };
        if reshaped || caret != state.last_caret || width != state.last_width {
            state.offset = reveal_caret(state.offset, caret, width, child_width);
            state.last_caret = caret;
            state.last_width = width;
        }
        let child = self.editor.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(child_width, limits.max().height)),
        );
        layout::Node::with_children(Size::new(width, child.size().height), vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.editor
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let size = layout.child(0).bounds().size();
        let max_offset = (size.width - bounds.width).max(0.0);
        let mut editor_cursor = if cursor.is_over(bounds) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        let mut drag_tick = None;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.dragging = cursor.is_over(bounds) && !shell.is_event_captured();
                state.last_drag_tick = None;
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Mouse(mouse::Event::CursorLeft)
            | Event::Window(window::Event::Unfocused) => {
                state.dragging = false;
                state.last_drag_tick = None;
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                if let Some(point) = cursor.position() {
                    editor_cursor = mouse::Cursor::Available(clamp_to_field(point, bounds));
                    if drag_step(point.x, bounds) != 0.0 && max_offset > 0.0 {
                        shell.request_redraw();
                    }
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) if state.dragging => {
                if let Some(point) = cursor.position() {
                    let step = drag_step(point.x, bounds);
                    let next = (state.offset + step).clamp(0.0, max_offset);
                    if next != state.offset {
                        let due = state
                            .last_drag_tick
                            .map_or(*now, |last| last + DRAG_INTERVAL);
                        if *now >= due {
                            state.offset = next;
                            state.last_drag_tick = Some(*now);
                            let position = clamp_to_field(point, bounds);
                            drag_tick = Some((
                                Event::Mouse(mouse::Event::CursorMoved { position }),
                                position,
                            ));
                            shell.request_redraw_at(*now + DRAG_INTERVAL);
                        } else {
                            shell.request_redraw_at(due);
                        }
                    }
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let pixels = match delta {
                    mouse::ScrollDelta::Lines { x, .. } => *x * 40.0,
                    mouse::ScrollDelta::Pixels { x, .. } => *x,
                };
                let next = (state.offset - pixels).clamp(0.0, max_offset);
                if next != state.offset {
                    state.offset = next;
                    shell.request_redraw();
                    shell.capture_event();
                }
                return;
            }
            _ => {}
        }
        let node = layout::Node::new(size);
        let child_layout = shifted_layout(&node, bounds, state.offset);
        let editor = self.editor.as_widget_mut();
        let tree = &mut tree.children[0];
        if let Some((event, position)) = drag_tick {
            editor.update(
                tree,
                &event,
                child_layout,
                mouse::Cursor::Available(position),
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
        editor.update(
            tree,
            event,
            child_layout,
            editor_cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) && cursor.is_over(*viewport) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use renderer::Renderer as _;
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<State>();
        let node = layout::Node::new(layout.child(0).bounds().size());
        renderer.with_layer(clip, |renderer| {
            self.editor.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                shifted_layout(&node, bounds, state.offset),
                cursor,
                &clip,
            );
        });
    }
}

fn shifted_layout(node: &layout::Node, bounds: Rectangle, offset: f32) -> Layout<'_> {
    Layout::with_offset(Vector::new(bounds.x - offset, bounds.y), node)
}

fn reveal_caret(offset: f32, caret: f32, width: f32, content: f32) -> f32 {
    let margin = CARET_MARGIN.min(width / 2.0);
    offset
        .max(caret + margin - width)
        .min(caret - margin)
        .clamp(0.0, (content - width).max(0.0))
}

fn drag_step(x: f32, bounds: Rectangle) -> f32 {
    let edge = DRAG_EDGE.min(bounds.width / 3.0);
    let left = bounds.x + edge;
    let right = bounds.x + bounds.width - edge;
    if x < left {
        -((left - x) * 0.8).clamp(1.0, 40.0)
    } else if x > right {
        ((x - right) * 0.8).clamp(1.0, 40.0)
    } else {
        0.0
    }
}

fn clamp_to_field(point: Point, bounds: Rectangle) -> Point {
    Point::new(
        point
            .x
            .clamp(bounds.x, bounds.x + (bounds.width - 0.1).max(0.0)),
        bounds.center_y(),
    )
}

#[cfg(test)]
mod tests {
    use iced::advanced::{clipboard, renderer::Headless};

    use crate::message::RequestMsg;

    use super::*;

    #[tokio::test]
    async fn holding_the_drag_at_the_edge_selects_the_hidden_suffix() {
        let renderer = iced::Renderer::new(MONO, TEXT_MD.into(), Some("tiny-skia"))
            .await
            .unwrap();
        let mut content = text_editor::Content::with_text(&format!(
            "https://example.test/{}hidden-tail",
            "x".repeat(80)
        ));
        let mut tree = Tree::empty();
        let bounds = Size::new(100.0, 36.0);
        let mut drive = |event: Event, position: Point| {
            let editor = text_editor::TextEditor::new(&content)
                .padding(0)
                .font(MONO)
                .size(TEXT_MD)
                .wrapping(text::Wrapping::None)
                .on_action(|action| Message::Request(RequestMsg::UrlEdited(action)));
            let mut field = url_viewport(editor, "tab", &content);
            tree.diff(field.as_widget());
            let node = field.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, bounds),
            );
            let mut messages = Vec::new();
            field.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Available(position),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &Rectangle::new(Point::ORIGIN, bounds),
            );
            drop(field);
            for message in messages {
                if let Message::Request(RequestMsg::UrlEdited(action)) = message {
                    content.perform(action);
                }
            }
        };
        drive(
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Point::new(16.0, 6.0),
        );
        let edge = Point::new(120.0, 6.0);
        drive(
            Event::Mouse(mouse::Event::CursorMoved { position: edge }),
            edge,
        );
        let start = iced::time::Instant::now();
        for frame in 0..60 {
            drive(
                Event::Window(window::Event::RedrawRequested(
                    start + DRAG_INTERVAL * frame,
                )),
                edge,
            );
        }
        let selected = content.selection().unwrap();
        assert!(selected.ends_with("hidden-tail"), "{selected}");
        assert!(selected.len() > 80, "{selected}");
    }
}
