import AppKit
import SwiftUI

/// The board's colors, matched to Nora's window in dark and light appearance.
enum GitHubBoardStyle {
    static let background = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x1B1B1B), light: GitHubPanelStyle.rgb(0xF6F6F5))
    static let card = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x242424), light: .white)
    static let cardHover = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x292929), light: GitHubPanelStyle.rgb(0xFCFCFC))
    static let cardBorder = GitHubPanelStyle.appearanceColor(dark: NSColor(white: 1, alpha: 0.06), light: NSColor(white: 0, alpha: 0.08))
    static let cardBorderHover = GitHubPanelStyle.appearanceColor(dark: NSColor(white: 1, alpha: 0.12), light: NSColor(white: 0, alpha: 0.14))
    static let control = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x242424), light: .white)
    static let controlBorder = GitHubPanelStyle.appearanceColor(dark: NSColor(white: 1, alpha: 0.07), light: NSColor(white: 0, alpha: 0.1))
    static let selectedSegment = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x3A3A3A), light: .white)
    static let labelFill = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x2E2E2E), light: GitHubPanelStyle.rgb(0xF0F0EF))
    static let divider = GitHubPanelStyle.appearanceColor(dark: NSColor(white: 1, alpha: 0.07), light: NSColor(white: 0, alpha: 0.08))
    static let drawer = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x1F1F1F), light: .white)
    static let scrim = GitHubPanelStyle.appearanceColor(dark: NSColor(white: 0, alpha: 0.35), light: NSColor(white: 0, alpha: 0.12))
    /// Open issues and pull requests, as GitHub marks them.
    static let open = GitHubPanelStyle.appearanceColor(dark: GitHubPanelStyle.rgb(0x5FCF80), light: GitHubPanelStyle.rgb(0x1A7F37))

    static let cornerRadius: CGFloat = 12
    static let columnWidth: CGFloat = 316
    static let drawerWidth: CGFloat = 680

    /// The color a repository's tile takes, the same for it every time.
    static func tint(for repository: String) -> Color {
        let tints: [UInt32] = [0x9EC5FF, 0x8FD9A8, 0xF19CCF, 0xF5C06E, 0xB8A3F5, 0x7FD6D0]
        let hash = repository.lowercased().unicodeScalars.reduce(UInt32(5381)) { ($0 &* 33) &+ $1.value }
        return Color(nsColor: GitHubPanelStyle.rgb(tints[Int(hash % UInt32(tints.count))]))
    }

    /// A label's own color from GitHub's hex, for the small dot beside its name.
    static func labelColor(_ hex: String) -> Color {
        guard hex.count == 6, let value = UInt32(hex, radix: 16) else { return .secondary }
        return Color(nsColor: GitHubPanelStyle.rgb(value))
    }
}
