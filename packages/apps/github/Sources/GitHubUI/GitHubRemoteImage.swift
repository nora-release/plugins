import AppKit
import GitHubCore
import SwiftUI

private struct GitHubAvatarsKey: EnvironmentKey {
    static let defaultValue: GitHubAvatars? = nil
}

extension EnvironmentValues {
    /// Where the panel's avatars come from; without it, every avatar shows its placeholder.
    var githubAvatars: GitHubAvatars? {
        get { self[GitHubAvatarsKey.self] }
        set { self[GitHubAvatarsKey.self] = newValue }
    }
}

/// An avatar fetched through Nora, the placeholder showing until it arrives or when it can't.
struct GitHubRemoteImage<Placeholder: View>: View {
    let url: URL?
    @ViewBuilder let placeholder: Placeholder
    @Environment(\.githubAvatars) private var avatars
    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image { Image(nsImage: image).resizable().scaledToFill() } else { placeholder }
        }
        .task(id: url) {
            image = nil
            guard let url, let avatars, let data = await avatars.data(url), !Task.isCancelled else { return }
            image = NSImage(data: data)
        }
    }
}
