import Foundation

/// GitHub's answers as the agent reads them. An answer repeats every account and repository in
/// full and links each object to a dozen API addresses; left in, they cost more of the agent's
/// context than the content does.
extension GitHubValue {
    private static let repositorySummaryKeys = [
        "full_name", "description", "private", "fork", "archived", "default_branch", "language",
        "pushed_at", "open_issues_count", "permissions", "html_url",
    ]

    /// The value without empty fields and API links. An account or a repository inside another
    /// object becomes its login or a short summary; the object asked for keeps its own fields.
    public func compact() -> GitHubValue { compact(nested: false) }

    private func compact(nested: Bool) -> GitHubValue {
        switch self {
        case .array(let rows):
            return .array(rows.map { $0.compact(nested: nested) })
        case .object(let fields):
            if nested, let login = accountLogin { return .string(login) }
            if nested, isRepository { return repositorySummary }
            return .object(fields.filter { !Self.isNoise($0.key, $0.value) }.mapValues { $0.compact(nested: true) })
        default:
            return self
        }
    }

    /// The few fields that tell one repository from another.
    public var repositorySummary: GitHubValue { selecting(Self.repositorySummaryKeys) }

    /// The object with only these fields, leaving out the ones it does not have.
    public func selecting(_ keys: [String]) -> GitHubValue {
        var result: [String: GitHubValue] = [:]
        for key in keys where !self[key].isNull { result[key] = self[key] }
        return .object(result)
    }

    /// Every text longer than the limit cut to it, with a mark that says so.
    func shorteningText(maximumBytes: Int, mark: String) -> GitHubValue {
        switch self {
        case .string(let text) where text.utf8.count > maximumBytes:
            .string(GitHubService.boundedUTF8(text, maximumBytes: maximumBytes) + mark)
        case .array(let rows):
            .array(rows.map { $0.shorteningText(maximumBytes: maximumBytes, mark: mark) })
        case .object(let fields):
            .object(fields.mapValues { $0.shorteningText(maximumBytes: maximumBytes, mark: mark) })
        default:
            self
        }
    }

    private var accountLogin: String? { self["avatar_url"].isNull ? nil : self["login"].string }
    private var isRepository: Bool { self["full_name"].string != nil && !self["owner"].isNull }

    private static func isNoise(_ key: String, _ value: GitHubValue) -> Bool {
        if value.isNull || ["node_id", "gravatar_id", "avatar_url", "_links"].contains(key) { return true }
        if key == "reactions" { return value["total_count"].int == 0 }
        guard key == "url" || key.hasSuffix("_url") else { return false }
        return value.text.hasPrefix("https://api.github.com/")
    }
}
