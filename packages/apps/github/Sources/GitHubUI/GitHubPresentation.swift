import AppKit
import GitHubCore
import SwiftUI

extension GitHubValue {
    var title: String { self["title"].text }
    var login: String { self["login"].string ?? self["name"].text }
    var repositoryName: String {
        if let value = self["repository"]["full_name"].string { return value }
        if let value = self["base"]["repo"]["full_name"].string { return value }
        guard let url = URL(string: self["repository_url"].text) else { return "" }
        return url.path.split(separator: "/").suffix(2).joined(separator: "/")
    }
    var itemKey: String { "\(repositoryName)#\(self["number"].int ?? 0)" }
    var isPullRequest: Bool { self["pull_request"] != .null || self["head"] != .null || self["draft"] != .null }
    var person: Self {
        if self["user"] != .null { return self["user"] }
        if self["actor"] != .null { return self["actor"] }
        return self["author"]
    }
    var eventDate: Date? {
        self["submitted_at"].date ?? self["created_at"].date ?? self["commit"]["committer"]["date"].date
            ?? self["committer"]["date"].date ?? self["commit"]["author"]["date"].date ?? self["author"]["date"].date
    }
    var shortSHA: String {
        let sha = self["sha"].string ?? self["commit_id"].string ?? self["after_commit"]["sha"].text
        return String(sha.prefix(8))
    }
    var commitTitle: String {
        let message = self["message"].string ?? self["commit"]["message"].text
        return String(message.split(separator: "\n", maxSplits: 1).first ?? "")
    }
    var statusIcon: GitHubIconKind {
        if self["merged"].bool == true || self["merged_at"].string != nil || self["pull_request"]["merged_at"].string != nil { return .merged }
        if self["state"].text == "closed" { return .closed }
        if self["draft"].bool == true { return .draft }
        return isPullRequest ? .open : .issue
    }
    var statusColor: Color {
        switch statusIcon { case .merged: .purple; case .closed: .red; case .draft: .secondary; default: .green }
    }
}

struct GitHubAvatar: View {
    let person: GitHubValue
    var size: CGFloat = 24
    private var url: URL? {
        if let value = person["avatar_url"].string, let url = URL(string: value), url.scheme == "https" { return url }
        guard !person.login.isEmpty, !person.login.contains(" ") else { return nil }
        return URL(string: "https://github.com/\(person.login).png?size=64")
    }
    var body: some View {
        AsyncImage(url: url) { phase in
            if case let .success(image) = phase { image.resizable().scaledToFill() }
            else {
                ZStack {
                    Color.secondary.opacity(0.12)
                    Text(String(person.login.prefix(1)).uppercased()).font(.system(size: size * 0.45, weight: .medium)).foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: size, height: size).clipShape(Circle())
        .help(person.login).accessibilityLabel(person.login)
    }
}

struct GitHubDate: View {
    let date: Date?
    var relative = false
    var body: some View {
        if let date {
            Group {
                if relative { Text(date, style: .relative) }
                else { Text(date, format: .dateTime.year().month().day()) }
            }
            .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
            .help(date.formatted(date: .long, time: .shortened))
        }
    }
}

struct GitHubChangeCount: View {
    let item: GitHubValue
    var body: some View {
        HStack(spacing: 8) {
            if let additions = item["additions"].int { Text("+\(additions)").foregroundStyle(.green) }
            if let deletions = item["deletions"].int { Text("−\(deletions)").foregroundStyle(.red) }
        }.font(.system(size: 11, weight: .medium)).monospacedDigit().fixedSize()
    }
}

struct GitHubSkeleton: View {
    var rows = 6
    var sidebar = false
    var inset: CGFloat?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var glowing = false
    var body: some View {
        VStack(alignment: .leading, spacing: sidebar ? 18 : 24) {
            ForEach(0 ..< rows, id: \.self) { index in
                HStack(alignment: .top, spacing: 10) {
                    Circle().fill(.quaternary).frame(width: sidebar ? 24 : 30, height: sidebar ? 24 : 30)
                    VStack(alignment: .leading, spacing: 8) {
                        RoundedRectangle(cornerRadius: 3).fill(.quaternary).frame(height: 10)
                        RoundedRectangle(cornerRadius: 3).fill(.quaternary)
                            .frame(maxWidth: sidebar ? 100 : CGFloat(170 + index % 3 * 70))
                            .frame(height: 8).frame(maxWidth: .infinity, alignment: .leading)
                        if !sidebar { RoundedRectangle(cornerRadius: 3).fill(.quaternary).frame(height: 50) }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(inset ?? (sidebar ? 12 : 24))
        .transaction { $0.animation = nil }
        .animation(reduceMotion ? nil : .easeInOut(duration: 1).repeatForever(autoreverses: true)) { content in
            content.opacity(reduceMotion ? 0.6 : glowing ? 0.8 : 0.4)
        }
        .onAppear { glowing = true }
        .accessibilityElement(children: .ignore).accessibilityLabel(GHL("Loading GitHub content"))
    }
}

@MainActor
enum GitHubFileIcon {
    private static var images: [String: NSImage] = [:]
    private struct Language: Decodable {
        let id: String
        let aliases: [String]?
        let icon: String?
    }
    private static let languages: [Language] = {
        guard let url = Bundle.module.url(forResource: "languages", withExtension: "json", subdirectory: "FileIcons"),
              let data = try? Data(contentsOf: url) else { return [] }
        return (try? JSONDecoder().decode([Language].self, from: data)) ?? []
    }()
    static func image(fileName: String) -> NSImage {
        let language = GitHubSourceDocument.language(fileName: fileName) ?? ""
        let name = languages.first { $0.id == language || $0.aliases?.contains(language) == true }?.icon ?? "file"
        if let image = images[name] { return image }
        let image = Bundle.module.url(forResource: name, withExtension: "pdf", subdirectory: "FileIcons").flatMap(NSImage.init(contentsOf:))
            ?? NSImage(systemSymbolName: "doc", accessibilityDescription: nil) ?? NSImage()
        images[name] = image
        return image
    }
}
