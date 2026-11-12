import Foundation
import GitHubCore

/// Which items the board shows.
enum GitHubBoardFilter: String, CaseIterable, Identifiable {
    case all, issues, pullRequests

    var id: Self { self }

    var title: String {
        switch self {
        case .all: GHL("All")
        case .issues: GHL("Issues")
        case .pullRequests: GHL("Pull requests")
        }
    }

    func includes(_ item: GitHubValue) -> Bool {
        switch self {
        case .all: true
        case .issues: !item.isPullRequest
        case .pullRequests: item.isPullRequest
        }
    }
}

/// The board's columns: one per repository, its items newest first, and the repository whose
/// newest item changed last first.
struct GitHubBoardColumn: Identifiable, Equatable {
    let repository: String
    let items: [GitHubValue]

    var id: String { repository.lowercased() }
    var owner: String { repository.split(separator: "/").first.map(String.init) ?? "" }
    var name: String { repository.split(separator: "/").dropFirst().first.map(String.init) ?? repository }

    static func columns(_ items: [GitHubValue], filter: GitHubBoardFilter, query: String) -> [Self] {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let shown = items.filter { item in
            filter.includes(item) && (query.isEmpty || item.title.localizedCaseInsensitiveContains(query)
                || item.repositoryName.localizedCaseInsensitiveContains(query)
                || String(item["number"].int ?? 0).contains(query))
        }
        // Each date is read once: sorting compares every item many times.
        let dated = shown.map { (item: $0, updated: updated($0)) }
        let grouped = Dictionary(grouping: dated) { $0.item.repositoryName.lowercased() }
        return grouped.values.compactMap { group -> (column: Self, updated: Date)? in
            let sorted = group.sorted { $0.updated > $1.updated }
            guard let first = sorted.first else { return nil }
            return (Self(repository: first.item.repositoryName, items: sorted.map(\.item)), first.updated)
        }
        .sorted { left, right in
            left.updated == right.updated ? left.column.id < right.column.id : left.updated > right.updated
        }
        .map(\.column)
    }

    static func updated(_ item: GitHubValue) -> Date {
        item["updated_at"].date ?? item["created_at"].date ?? .distantPast
    }

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.repository == rhs.repository && lhs.items.map(\.itemKey) == rhs.items.map(\.itemKey)
    }
}

extension GitHubValue {
    /// The labels a card shows: name and color, at most `limit`, and how many more there are.
    func boardLabels(limit: Int = 3) -> (labels: [(name: String, color: String)], more: Int) {
        guard case .array(let rows) = self["labels"] else { return ([], 0) }
        let labels = rows.compactMap { row -> (name: String, color: String)? in
            let name = row["name"].text
            return name.isEmpty ? nil : (name, row["color"].text)
        }
        return (Array(labels.prefix(limit)), max(0, labels.count - limit))
    }
}
