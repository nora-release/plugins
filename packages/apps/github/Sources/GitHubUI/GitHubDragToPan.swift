import AppKit
import SwiftUI

/// Dragging a scroll view moves it as a canvas moves: grab anywhere, the empty space or a card, and pull.
/// A short click still reaches what is under it, and the trackpad scrolls as before.
struct GitHubDragToPan: ViewModifier {
    let axis: Axis
    @State private var position = ScrollPosition()
    @State private var bounds = Bounds()
    @State private var origin: CGPoint?

    /// Where the scroll view is, and how far it can go each way.
    struct Bounds: Equatable {
        var offset = CGPoint.zero
        var minimum = CGPoint.zero
        var maximum = CGPoint.zero

        init() {}

        init(_ geometry: ScrollGeometry) {
            offset = geometry.contentOffset
            minimum = CGPoint(x: -geometry.contentInsets.leading, y: -geometry.contentInsets.top)
            maximum = CGPoint(x: max(minimum.x, geometry.contentSize.width + geometry.contentInsets.trailing - geometry.containerSize.width),
                              y: max(minimum.y, geometry.contentSize.height + geometry.contentInsets.bottom - geometry.containerSize.height))
        }

        /// Where a drag that began at `origin` and has moved by `translation` takes the scroll view.
        func target(from origin: CGPoint, by translation: CGSize, along axis: Axis) -> CGPoint {
            switch axis {
            case .horizontal: CGPoint(x: min(max(origin.x - translation.width, minimum.x), maximum.x), y: offset.y)
            case .vertical: CGPoint(x: offset.x, y: min(max(origin.y - translation.height, minimum.y), maximum.y))
            }
        }
    }

    func body(content: Content) -> some View {
        content
            .scrollPosition($position)
            .onScrollGeometryChange(for: Bounds.self, of: Bounds.init) { _, new in bounds = new }
            .contentShape(Rectangle())
            .simultaneousGesture(
                DragGesture(minimumDistance: 4, coordinateSpace: .global)
                    .onChanged { drag in
                        if origin == nil {
                            origin = bounds.offset
                            NSCursor.closedHand.push()
                        }
                        guard let origin else { return }
                        position.scrollTo(point: bounds.target(from: origin, by: drag.translation, along: axis))
                    }
                    .onEnded { _ in release() }
            )
            .onDisappear(perform: release)
    }

    private func release() {
        guard origin != nil else { return }
        origin = nil
        NSCursor.pop()
    }
}

extension View {
    /// Lets the scroll view be dragged along `axis`, as a canvas is.
    func dragToPan(_ axis: Axis) -> some View { modifier(GitHubDragToPan(axis: axis)) }
}
