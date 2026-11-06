import AppKit
import SwiftUI

struct GitHubMarkdownTableView: View {
    let table: GitHubMarkdownTable
    let path: String
    @State private var viewportWidth: CGFloat = 0

    var body: some View {
        let widths = columnWidths
        ScrollView(.horizontal) {
            VStack(spacing: 0) {
                if !table.headers.isEmpty {
                    tableRow(table.headers, index: 0, widths: widths, isHeader: true)
                }
                ForEach(Array(table.rows.enumerated()), id: \.offset) { index, row in
                    if !table.headers.isEmpty || index > 0 {
                        Divider()
                    }
                    tableRow(row, index: index + 1, widths: widths, isHeader: false)
                }
            }
            .frame(width: widths.reduce(0, +))
            .background(Color.primary.opacity(0.018))
        }
        .frame(maxWidth: .infinity)
        .background {
            GeometryReader { proxy in
                Color.clear.preference(key: GitHubMarkdownTableWidthKey.self, value: proxy.size.width)
            }
        }
        .onPreferenceChange(GitHubMarkdownTableWidthKey.self) { width in
            if abs(width - viewportWidth) > 0.5 {
                viewportWidth = width
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay {
            RoundedRectangle(cornerRadius: 6).strokeBorder(Color.primary.opacity(0.1))
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("github.markdown.table.\(path)")
    }

    private var columnWidths: [CGFloat] {
        let count = max(table.headers.count, table.rows.map(\.count).max() ?? 0)
        guard count > 0 else { return [] }
        let preferred = (0 ..< count).map { index -> CGFloat in
            let header = table.headers.indices.contains(index) ? [table.headers[index]] : []
            let values = header + table.rows.prefix(24).compactMap { $0.indices.contains(index) ? $0[index] : nil }
            let font = NSFont.systemFont(ofSize: 13)
            let width = values.map { value in
                let text = String(GitHubMarkdownView.attributedText(value).characters.prefix(180))
                return (text as NSString).size(withAttributes: [.font: font]).width
            }.max() ?? 0
            return min(320, max(100, ceil(width) + 28))
        }
        let total = preferred.reduce(0, +)
        guard viewportWidth > total else { return preferred }
        let extra = (viewportWidth - total) / CGFloat(count)
        return preferred.map { $0 + extra }
    }

    private func tableRow(_ values: [String], index: Int, widths: [CGFloat], isHeader: Bool) -> some View {
        HStack(alignment: .top, spacing: 0) {
            ForEach(Array(widths.enumerated()), id: \.offset) { column, width in
                GitHubMarkdownInlineText(source: values.indices.contains(column) ? values[column] : "")
                    .font(.system(size: 13, weight: isHeader ? .semibold : .regular))
                    .frame(maxWidth: .infinity, alignment: alignment(for: column))
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .frame(width: width, alignment: alignment(for: column))
                    .accessibilityIdentifier("github.markdown.table.\(path).cell.\(index).\(column)")
            }
        }
        .background(isHeader ? Color.primary.opacity(0.045) : .clear)
    }

    private func alignment(for column: Int) -> Alignment {
        guard table.alignments.indices.contains(column) else { return .leading }
        return switch table.alignments[column] {
        case .left: .leading
        case .center: .center
        case .right: .trailing
        }
    }
}

private struct GitHubMarkdownTableWidthKey: PreferenceKey {
    static var defaultValue: CGFloat {
        0
    }

    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}
