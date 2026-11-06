import SwiftUI

struct GitHubMarkdownBlockList: View {
    let blocks: [GitHubMarkdownBlock]
    let path: String
    var spacing: CGFloat = 16

    var body: some View {
        VStack(alignment: .leading, spacing: spacing) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { index, block in
                GitHubMarkdownBlockView(block: block, path: "\(path).\(index)", isFirst: index == 0)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct GitHubMarkdownBlockView: View {
    let block: GitHubMarkdownBlock
    let path: String
    let isFirst: Bool

    var body: some View {
        switch block {
        case let .paragraph(text):
            GitHubMarkdownInlineText(source: text)
                .accessibilityIdentifier("github.markdown.paragraph.\(path)")
        case let .heading(level, text):
            VStack(alignment: .leading, spacing: 8) {
                GitHubMarkdownInlineText(source: text)
                    .font(.system(size: headingSize(level), weight: .semibold))
                    .accessibilityAddTraits(.isHeader)
                if level <= 2 {
                    Divider().accessibilityHidden(true)
                }
            }
            .padding(.top, !isFirst && level <= 2 ? 8 : 0)
        case let .image(source, alt):
            if let url = URL(string: source), url.scheme?.lowercased() == "https" {
                AsyncImage(url: url) { phase in
                    switch phase {
                    case let .success(image): image.resizable().scaledToFit().frame(maxHeight: 600)
                    case .failure: Label(alt.isEmpty ? GHL("Image unavailable") : alt, systemImage: "photo").foregroundStyle(.secondary)
                    default: RoundedRectangle(cornerRadius: 4).fill(.quaternary).frame(height: 140)
                    }
                }.accessibilityLabel(alt)
            }
        case let .code(language, text):
            GitHubMarkdownCodeView(language: language, source: text, path: path)
        case let .quote(children):
            GitHubMarkdownBlockList(blocks: children, path: path, spacing: 10)
                .foregroundStyle(.secondary)
                .padding(.leading, 14)
                .overlay(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 1)
                        .fill(Color.secondary.opacity(0.35))
                        .frame(width: 3)
                }
        case let .callout(kind, children):
            GitHubMarkdownCalloutView(kind: kind, children: children, path: path)
        case let .list(orderedStart, items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, children in
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Text(verbatim: orderedStart.map { "\($0 + index)." } ?? "•")
                            .foregroundStyle(.secondary)
                            .frame(minWidth: orderedStart == nil ? 12 : 22, alignment: .trailing)
                            .accessibilityHidden(true)
                        GitHubMarkdownBlockList(blocks: children, path: "\(path).\(index)", spacing: 8)
                    }
                }
            }
        case let .taskList(items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    HStack(alignment: .top, spacing: 10) {
                        Toggle(isOn: .constant(item.isCompleted)) { EmptyView() }
                            .labelsHidden()
                            .toggleStyle(.checkbox)
                            .disabled(true)
                            .accessibilityLabel(taskLabel(item.children))
                            .accessibilityValue(item.isCompleted ? GHL("Completed") : GHL("Incomplete"))
                            .accessibilityIdentifier("github.markdown.task.\(path).\(index)")
                        GitHubMarkdownBlockList(blocks: item.children, path: "\(path).\(index)", spacing: 8)
                    }
                }
            }
        case let .table(table):
            GitHubMarkdownTableView(table: table, path: path)
        case let .disclosure(title, children, isExpanded):
            GitHubMarkdownDisclosureView(title: title, children: children, initiallyExpanded: isExpanded, path: path)
        case .rule:
            Divider().padding(.vertical, 2)
        }
    }

    private func headingSize(_ level: Int) -> CGFloat {
        switch level {
        case 1: 24
        case 2: 20
        case 3: 16
        default: 14
        }
    }

    private func taskLabel(_ children: [GitHubMarkdownBlock]) -> String {
        guard case let .paragraph(text)? = children.first else { return GHL("Task") }
        return String(GitHubMarkdownView.attributedText(text).characters)
    }
}

private struct GitHubMarkdownCalloutView: View {
    let kind: String
    let children: [GitHubMarkdownBlock]
    let path: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: symbol)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(tint)
            GitHubMarkdownBlockList(blocks: children, path: path, spacing: 10)
        }
        .padding(12)
        .padding(.leading, 3)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.055), in: RoundedRectangle(cornerRadius: 6))
        .overlay(alignment: .leading) {
            RoundedRectangle(cornerRadius: 1)
                .fill(tint.opacity(0.75))
                .frame(width: 3)
                .padding(.vertical, 4)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("github.markdown.callout.\(path)")
    }

    private var title: String {
        switch kind.uppercased() {
        case "TIP": GHL("Tip")
        case "IMPORTANT": GHL("Important")
        case "WARNING": GHL("Warning")
        case "CAUTION": GHL("Caution")
        default: GHL("Note")
        }
    }

    private var symbol: String {
        switch kind.uppercased() {
        case "TIP": "lightbulb"
        case "IMPORTANT": "exclamationmark.bubble"
        case "WARNING", "CAUTION": "exclamationmark.triangle"
        default: "info.circle"
        }
    }

    private var tint: Color {
        switch kind.uppercased() {
        case "TIP": .green
        case "IMPORTANT": .purple
        case "WARNING": .orange
        case "CAUTION": .red
        default: .blue
        }
    }
}

private struct GitHubMarkdownDisclosureView: View {
    let title: String
    let children: [GitHubMarkdownBlock]
    let path: String
    @State private var isExpanded: Bool

    init(title: String, children: [GitHubMarkdownBlock], initiallyExpanded: Bool, path: String) {
        self.title = title
        self.children = children
        self.path = path
        _isExpanded = State(initialValue: initiallyExpanded)
    }

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            GitHubMarkdownBlockList(blocks: children, path: path)
                .padding(.top, 12)
        } label: {
            GitHubMarkdownInlineText(source: title)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("github.markdown.disclosure.\(path)")
    }
}
