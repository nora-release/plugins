import AppKit
import SwiftUI

enum GitHubPanelStyle {
    static let background = Color(nsColor: .textBackgroundColor)
    static let chrome = appearanceColor(
        dark: rgb(0x333333).withAlphaComponent(0.55),
        light: NSColor(white: 0.945, alpha: 0.55)
    )
    static let sidebar = appearanceColor(
        dark: rgb(0x212121).withAlphaComponent(0.6),
        light: NSColor(white: 0.98, alpha: 0.6)
    )

    private static func appearanceColor(dark: NSColor, light: NSColor) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light
        })
    }

    private static func rgb(_ value: UInt32) -> NSColor {
        NSColor(
            srgbRed: CGFloat((value >> 16) & 0xFF) / 255,
            green: CGFloat((value >> 8) & 0xFF) / 255,
            blue: CGFloat(value & 0xFF) / 255,
            alpha: 1
        )
    }
}

struct GitHubSidebarBackground: View {
    var body: some View {
        ZStack {
            GitHubSidebarVibrancy()
            GitHubPanelStyle.chrome
            GitHubPanelStyle.sidebar
        }
        .allowsHitTesting(false)
    }
}

private struct GitHubSidebarVibrancy: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .sidebar
        view.blendingMode = .behindWindow
        view.state = .followsWindowActiveState
        return view
    }

    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {}
}
