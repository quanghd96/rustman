//! The resize border of an undecorated window: a band along each edge that
//! shows the resize cursor and starts a native resize drag. It takes no
//! layout space; presses inside the band never reach the content.

use iced::advanced::widget::{Id, Operation, Tree};
use iced::advanced::widget::operation::{Scrollable, TextInput};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::window::Direction;
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};

/// Width of the band, in logical px.
pub const BORDER: f32 = 6.0;

/// The edge or corner `point` falls on in a window of `size`.
pub fn edge_at(point: Point, size: Size, border: f32) -> Option<Direction> {
    let left = point.x < border;
    let right = point.x >= size.width - border;
    let top = point.y < border;
    let bottom = point.y >= size.height - border;
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => Direction::NorthWest,
        (true, _, _, true) => Direction::NorthEast,
        (_, true, true, _) => Direction::SouthWest,
        (_, true, _, true) => Direction::SouthEast,
        (true, ..) => Direction::North,
        (_, true, ..) => Direction::South,
        (_, _, true, _) => Direction::West,
        (_, _, _, true) => Direction::East,
        _ => return None,
    })
}

/// The cursor shown over `direction`'s part of the band.
pub fn cursor(direction: Direction) -> mouse::Interaction {
    match direction {
        Direction::North | Direction::South => mouse::Interaction::ResizingVertically,
        Direction::East | Direction::West => mouse::Interaction::ResizingHorizontally,
        Direction::NorthEast | Direction::SouthWest => mouse::Interaction::ResizingDiagonallyUp,
        Direction::NorthWest | Direction::SouthEast => mouse::Interaction::ResizingDiagonallyDown,
    }
}

/// Wraps the whole window. With `enabled` off (maximized, or native
/// decorations) it is transparent.
pub fn resize_frame<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    enabled: bool,
    on_resize: fn(Direction) -> M,
    outside_press: Option<(Id, M)>,
) -> Element<'a, M> {
    Element::new(ResizeFrame {
        content: content.into(),
        enabled,
        on_resize,
        outside_press,
    })
}

struct ResizeFrame<'a, M> {
    content: Element<'a, M>,
    enabled: bool,
    on_resize: fn(Direction) -> M,
    outside_press: Option<(Id, M)>,
}

/// Checks whether a press hit the named input, including inside a scrolled tree.
struct InputHit<'a> {
    id: &'a Id,
    point: Point,
    next_scroll: Option<(Rectangle, Vector)>,
    hit: bool,
}

impl Operation for InputHit<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        if self.hit {
            return;
        }
        let original = self.point;
        if let Some((bounds, translation)) = self.next_scroll.take() {
            if !bounds.contains(original) {
                return;
            }
            self.point = original + translation;
        }
        operate(self);
        self.point = original;
    }

    fn scrollable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        translation: Vector,
        _: &mut dyn Scrollable,
    ) {
        self.next_scroll = Some((bounds, translation));
    }

    fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, _: &mut dyn TextInput) {
        self.hit |= id == Some(self.id) && bounds.contains(self.point);
    }
}

impl<M> ResizeFrame<'_, M> {
    fn edge(&self, layout: Layout<'_>, cursor: mouse::Cursor) -> Option<Direction> {
        if !self.enabled {
            return None;
        }
        let bounds = layout.bounds();
        let at = cursor.position_in(bounds)?;
        edge_at(at, bounds.size(), BORDER)
    }
}

impl<M: Clone> Widget<M, iced::Theme, iced::Renderer> for ResizeFrame<'_, M> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &iced::Renderer, operation: &mut dyn Operation) {
        self.content
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
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if let Some((id, message)) = &self.outside_press
            && let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left | mouse::Button::Right)) = event
            && let Some(point) = cursor.position()
        {
            let mut probe = InputHit {
                id,
                point,
                next_scroll: None,
                hit: false,
            };
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, &mut probe);
            if !probe.hit {
                // Finish before the clicked control runs, without taking its click.
                shell.publish(message.clone());
            }
        }
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && let Some(direction) = self.edge(layout, cursor)
        {
            shell.publish((self.on_resize)(direction));
            shell.capture_event();
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        match self.edge(layout, cursor) {
            Some(direction) => self::cursor(direction),
            None => self
                .content
                .as_widget()
                .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer),
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
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, iced::Theme, iced::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Size = Size::new(1440.0, 900.0);

    #[test]
    fn edges_and_corners_map_to_their_directions() {
        let at = |x, y| edge_at(Point::new(x, y), WINDOW, BORDER);
        assert!(matches!(at(2.0, 2.0), Some(Direction::NorthWest)));
        assert!(matches!(at(1438.0, 1.0), Some(Direction::NorthEast)));
        assert!(matches!(at(0.0, 899.0), Some(Direction::SouthWest)));
        assert!(matches!(at(1439.0, 899.0), Some(Direction::SouthEast)));
        assert!(matches!(at(700.0, 3.0), Some(Direction::North)));
        assert!(matches!(at(700.0, 896.0), Some(Direction::South)));
        assert!(matches!(at(1.0, 450.0), Some(Direction::West)));
        assert!(matches!(at(1437.0, 450.0), Some(Direction::East)));
        assert!(at(700.0, 450.0).is_none());
        assert!(at(6.0, 6.0).is_none(), "the band is exactly BORDER wide");
    }

    #[test]
    fn diagonal_cursors_follow_the_corner() {
        assert_eq!(cursor(Direction::NorthEast), mouse::Interaction::ResizingDiagonallyUp);
        assert_eq!(cursor(Direction::SouthEast), mouse::Interaction::ResizingDiagonallyDown);
        assert_eq!(cursor(Direction::West), mouse::Interaction::ResizingHorizontally);
    }

    #[test]
    fn click_away_checks_the_visible_position_of_the_rename_input() {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let id = Id::new("rename");
        let mut input = iced::widget::text_input::State::<Paragraph>::new();
        let tree_viewport = Rectangle::new(Point::new(0.0, 80.0), Size::new(260.0, 220.0));
        let input_bounds = Rectangle::new(Point::new(80.0, 240.0), Size::new(150.0, 24.0));
        for (point, hit) in [
            (Point::new(90.0, 146.0), true),
            (Point::new(90.0, 246.0), false),
            (Point::new(480.0, 146.0), false),
        ] {
            let mut probe = InputHit {
                id: &id,
                point,
                next_scroll: Some((tree_viewport, Vector::new(0.0, 100.0))),
                hit: false,
            };
            probe.traverse(&mut |operation| {
                operation.text_input(Some(&id), input_bounds, &mut input);
            });
            assert_eq!(probe.hit, hit);
        }
    }
}
