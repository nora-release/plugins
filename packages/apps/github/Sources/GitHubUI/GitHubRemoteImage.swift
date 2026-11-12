import AppKit
import GitHubCore
import SwiftUI

private struct GitHubImagesKey: EnvironmentKey {
    static let defaultValue: GitHubImages? = nil
}

extension EnvironmentValues {
    /// Where the panel's pictures come from; without it, every picture shows its placeholder.
    var githubImages: GitHubImages? {
        get { self[GitHubImagesKey.self] }
        set { self[GitHubImagesKey.self] = newValue }
    }
}

private struct GitHubRenderedImagesKey: EnvironmentKey {
    static let defaultValue: [GitHubImageSources.Rendered] = []
}

extension EnvironmentValues {
    /// The pictures of the body GitHub rendered for the Markdown being drawn.
    var githubRenderedImages: [GitHubImageSources.Rendered] {
        get { self[GitHubRenderedImagesKey.self] }
        set { self[GitHubRenderedImagesKey.self] = newValue }
    }
}

/// A picture GitHub serves, fetched through Nora; the placeholder shows while it loads, and is told
/// when it couldn't be had.
struct GitHubRemoteImage<Placeholder: View>: View {
    let url: URL?
    var fit = false
    @ViewBuilder let placeholder: (_ failed: Bool) -> Placeholder
    @Environment(\.githubImages) private var images
    @State private var image: NSImage?
    @State private var failed = false

    init(url: URL?, fit: Bool = false, @ViewBuilder placeholder: @escaping (_ failed: Bool) -> Placeholder) {
        self.url = url
        self.fit = fit
        self.placeholder = placeholder
    }

    init(url: URL?, @ViewBuilder placeholder: @escaping () -> Placeholder) {
        self.init(url: url) { _ in placeholder() }
    }

    var body: some View {
        Group {
            if let image {
                if fit { Image(nsImage: image).resizable().scaledToFit() } else { Image(nsImage: image).resizable().scaledToFill() }
            } else {
                placeholder(failed)
            }
        }
        .task(id: url) {
            image = nil; failed = false
            guard let url, let images else { failed = true; return }
            let data = await images.data(url)
            guard !Task.isCancelled else { return }
            if let data, let picture = NSImage(data: data) { image = picture } else { failed = true }
        }
    }
}
