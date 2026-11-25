//! Link hit-testing over the read-only code editor.

use std::ops::Range;

use iced::advanced::renderer::{Quad, Renderer as _};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};
use iced_code_editor::CodeEditor;
use unicode_width::UnicodeWidthChar;

use crate::message::{AppMsg, Message, RequestMsg, ResponseMsg};
use crate::services::response_links::{Link, LinkLine, PageLinks};
use crate::state::tabs::RequestTabState;
use crate::ui::theme::{EDITOR_PADDING, GUTTER_WIDTH, Palette};

const CLICK_SLOP: f32 = 4.0;
// The editor's rail is 12 px wide with a 2 px margin on each side.
const SCROLLBAR_SPACE: f32 = 16.0;
// Space the editor reserves on the right when it wraps to the viewport.
const WRAP_PADDING: f32 = 25.0;

pub(super) fn viewer(tab: &RequestTabState, external: bool) -> Element<'_, Message> {
    let editor = &tab.response_editor;
    let child = editor
        .view()
        .map(|msg| Message::Response(ResponseMsg::ViewerEdited(msg)));
    let (start, end) = tab.viewer_page_span();
    Element::new(LinkViewer {
        child,
        editor,
        links: &tab.viewer_links,
        text: &tab.viewer_full[start..end],
        external,
    })
}

struct LinkViewer<'a> {
    child: Element<'a, Message>,
    editor: &'a CodeEditor,
    links: &'a PageLinks,
    text: &'a str,
    external: bool,
}

#[derive(Default)]
struct State {
    pressed: Option<(Point, String)>,
    over_link: bool,
}

#[derive(Clone, Copy)]
struct Segment {
    line: u32,
    start: usize,
    end: usize,
}

impl LinkViewer<'_> {
    fn row_at(&self, y: f32) -> usize {
        ((y - EDITOR_PADDING) / self.editor.line_height()).max(0.0) as usize
    }

    fn row_center(&self, row: usize) -> f32 {
        EDITOR_PADDING + (row as f32 + 0.5) * self.editor.line_height()
    }

    fn editor_segment(&self, row: usize) -> Option<Segment> {
        let y = self.row_center(row);
        let left = self
            .editor
            .lsp_position_at_point(Point::new(GUTTER_WIDTH, y))?;
        let right = self.editor.lsp_position_at_point(Point::new(f32::MAX, y))?;
        (left.character < right.character).then_some(Segment {
            line: left.line,
            start: left.character as usize,
            end: right.character as usize,
        })
    }

    fn segments(
        &self,
        width: f32,
        rows: Range<usize>,
    ) -> impl Iterator<Item = (usize, Segment)> + '_ {
        // A body that fits the pane does not get a Scrolled event from iced.
        // Until that event, the editor hit-test uses its old width even
        // though the canvas already draws at the new one.
        let fallback =
            (width != self.editor.viewport_width()).then(|| self.rewrap(width, rows.end));
        rows.filter_map(move |row| {
            let segment = match &fallback {
                Some(segments) => segments.get(row).copied(),
                None => self.editor_segment(row),
            }?;
            Some((row, segment))
        })
    }

    fn rewrap(&self, width: f32, limit: usize) -> Vec<Segment> {
        let mut segments = Vec::new();
        let mut lines = self.text.split('\n').enumerate();
        let mut editor_row = 0;
        while segments.len() < limit {
            let Some(position) = self
                .editor
                .lsp_position_at_point(Point::new(GUTTER_WIDTH, self.row_center(editor_row)))
            else {
                break;
            };
            // The editor still knows which logical lines are visible, so
            // this skips folded lines even while its wrap width is stale.
            let Some((index, line)) = lines.find(|(index, _)| *index >= position.line as usize)
            else {
                break;
            };
            if index as u32 != position.line {
                break;
            }
            segments.extend(
                self.wrap_line(line, width)
                    .take(limit - segments.len())
                    .map(|range| Segment {
                        line: position.line,
                        start: range.start,
                        end: range.end,
                    }),
            );
            if segments.len() < limit {
                editor_row += self.wrap_line(line, self.editor.viewport_width()).count();
            }
        }
        segments
    }

    fn wrap_line<'a>(
        &'a self,
        line: &'a str,
        width: f32,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        let available = (width - GUTTER_WIDTH - WRAP_PADDING).max(self.editor.char_width());
        let mut start = 0;
        let mut used = 0.0;
        line.chars()
            .map(Some)
            .chain([None])
            .enumerate()
            .filter_map(move |(column, c)| {
                let Some(c) = c else {
                    return Some(start..column);
                };
                let advance = self.char_width(c);
                if column > start && used + advance - available >= 0.001 {
                    let range = start..column;
                    start = column;
                    used = advance;
                    Some(range)
                } else {
                    used += advance;
                    None
                }
            })
    }

    // Match the editor's cell geometry for ASCII, wide Unicode and tabs.
    fn char_width(&self, c: char) -> f32 {
        if c == '\t' {
            return self.editor.char_width() * 4.0;
        }
        match c.width() {
            Some(w) if w > 1 => self.editor.full_char_width(),
            Some(_) => self.editor.char_width(),
            None => 0.0,
        }
    }

    fn hit(
        &self,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) -> Option<(u32, &LinkLine, &Link)> {
        if self.links.is_empty() || !cursor.is_over(*viewport) {
            return None;
        }
        let point = cursor.position_in(bounds)?;
        let y = point.y + self.editor.viewport_scroll();
        let x = point.x;
        if y < EDITOR_PADDING || x < GUTTER_WIDTH || x >= bounds.width - SCROLLBAR_SPACE {
            return None;
        }
        let row = self.row_at(y);
        let (_, segment) = self.segments(bounds.width, row..row + 1).next()?;
        let line = self.links.line(segment.line)?;
        let mut edge = GUTTER_WIDTH;
        for (offset, c) in self.text[line.bytes.clone()]
            .chars()
            .skip(segment.start)
            .take(segment.end - segment.start)
            .enumerate()
        {
            edge += self.char_width(c);
            if x < edge {
                let col = segment.start + offset;
                let link = line.links.iter().find(|link| link.columns.contains(&col))?;
                return Some((segment.line, line, link));
            }
        }
        None
    }

    fn draw_hover(
        &self,
        renderer: &mut iced::Renderer,
        bounds: Rectangle,
        viewport: &Rectangle,
        cursor: mouse::Cursor,
    ) {
        let Some((line_index, line, link)) = self.hit(bounds, cursor, viewport) else {
            return;
        };
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let scroll = self.editor.viewport_scroll();
        let height = self.editor.line_height();
        let first = self.row_at(clip.y - bounds.y + scroll);
        let last = self.row_at(clip.y + clip.height - bounds.y + scroll);
        for (row, segment) in self.segments(bounds.width, first..last + 1) {
            if segment.line == line_index {
                let start = link.columns.start.max(segment.start);
                let end = link.columns.end.min(segment.end);
                if start < end {
                    let mut chars = self.text[line.bytes.clone()].chars().skip(segment.start);
                    let before: f32 = chars
                        .by_ref()
                        .take(start - segment.start)
                        .map(|c| self.char_width(c))
                        .sum();
                    let width: f32 = chars.take(end - start).map(|c| self.char_width(c)).sum();
                    let underline = Rectangle {
                        x: bounds.x + GUTTER_WIDTH + before,
                        y: bounds.y + self.row_center(row) - scroll + height / 2.0 - 2.0,
                        width,
                        height: 1.0,
                    };
                    if let Some(visible) = underline.intersection(&clip) {
                        renderer.fill_quad(
                            Quad {
                                bounds: visible,
                                ..Quad::default()
                            },
                            Palette::text(),
                        );
                    }
                }
            }
        }
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for LinkViewer<'_> {
    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.child.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.child)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.child));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.child
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.child
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
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        let mut follow = None;
        if !shell.is_event_captured() {
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    state.pressed = self
                        .hit(bounds, cursor, viewport)
                        .and_then(|(_, _, link)| Some((cursor.position()?, link.url.clone())));
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    if let Some((origin, url)) = state.pressed.take()
                        && cursor
                            .position()
                            .is_some_and(|point| origin.distance(point) <= CLICK_SLOP)
                        && self
                            .hit(bounds, cursor, viewport)
                            .is_some_and(|(_, _, link)| link.url == url)
                    {
                        follow = Some(url);
                    }
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    if state
                        .pressed
                        .as_ref()
                        .is_some_and(|(origin, _)| origin.distance(*position) > CLICK_SLOP)
                    {
                        state.pressed = None;
                    }
                    let over_link = self.hit(bounds, cursor, viewport).is_some();
                    if over_link || state.over_link {
                        shell.request_redraw();
                    }
                    state.over_link = over_link;
                }
                Event::Mouse(mouse::Event::CursorLeft)
                | Event::Window(iced::window::Event::Unfocused) => {
                    state.pressed = None;
                    if std::mem::take(&mut state.over_link) {
                        shell.request_redraw();
                    }
                }
                _ => {}
            }
        }
        // Let the editor finish its own selection gesture before switching tabs.
        self.child.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if let Some(url) = follow {
            let message = if self.external {
                Message::App(AppMsg::OpenUrl(url))
            } else {
                Message::Request(RequestMsg::OpenResponseLink(url))
            };
            shell.publish(message);
            shell.capture_event();
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
        self.child.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        self.draw_hover(renderer, layout.bounds(), viewport, cursor);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if self.hit(layout.bounds(), cursor, viewport).is_some() {
            mouse::Interaction::Pointer
        } else {
            self.child.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        }
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.child.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
