import Foundation

/// What the agent can do on GitHub through the plugin. Each case is a command in `nora-plugin.json`.
public enum GitHubCommand: String, CaseIterable, Sendable {
    case listRepositories, listIssues, listPullRequests, getIssue, getPullRequest, getPullRequestDiff, get
    case comment, createIssue, updateIssue, createPullRequest, reviewPullRequest, mergePullRequest

    public static let prefix = "com.nora.github."
    public var id: String { Self.prefix + rawValue }

    public init?(id: String) {
        guard id.hasPrefix(Self.prefix) else { return nil }
        self.init(rawValue: String(id.dropFirst(Self.prefix.count)))
    }

    /// Whether the command leaves GitHub as it found it.
    public var isReadOnly: Bool {
        switch self {
        case .listRepositories, .listIssues, .listPullRequests, .getIssue, .getPullRequest, .getPullRequestDiff, .get: true
        case .comment, .createIssue, .updateIssue, .createPullRequest, .reviewPullRequest, .mergePullRequest: false
        }
    }
}

public enum GitHubCommandError: Error, LocalizedError, Equatable, Sendable {
    case unknownCommand(String)
    case invalidArgument(String)
    case responseTooLarge

    public var errorDescription: String? {
        switch self {
        case .unknownCommand(let id): "Unsupported GitHub plugin command: \(id)."
        case .invalidArgument(let reason): reason
        case .responseTooLarge: "GitHub's answer is too large to return. Ask for less at a time, for example with per_page and page."
        }
    }
}

/// Runs the agent's GitHub commands as the account connected in the panel.
public struct GitHubCommands: Sendable {
    static let maximumResultBytes = 256 * 1024
    static let maximumDiffBytes = 128 * 1024
    static let shortenedMark = "\n… [shortened to fit]"
    private let api: GitHubAPI

    public init(api: GitHubAPI) { self.api = api }

    public func execute(commandID: String, arguments: GitHubValue) async throws -> GitHubValue {
        guard let command = GitHubCommand(id: commandID) else { throw GitHubCommandError.unknownCommand(commandID) }
        return try Self.bounded(await run(command, GitHubCommandArguments(values: arguments)))
    }

    private func run(_ command: GitHubCommand, _ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        switch command {
        case .listRepositories:
            try await Self.page(api.repositories(page: arguments.page()), row: \.repositorySummary)
        case .listIssues:
            try await Self.page(api.issues(repository: arguments.repository(), state: arguments.state(), page: arguments.page()), row: Self.itemSummary)
        case .listPullRequests:
            try await Self.page(api.pullRequests(repository: arguments.repository(), state: arguments.state(), page: arguments.page()), row: Self.itemSummary)
        case .getIssue: try await issue(arguments)
        case .getPullRequest: try await pullRequest(arguments)
        case .getPullRequestDiff: try await pullRequestDiff(arguments)
        case .get: try await api.request(path: arguments.apiPath()).data.compact()
        case .comment: try await comment(arguments)
        case .createIssue: try await createIssue(arguments)
        case .updateIssue: try await updateIssue(arguments)
        case .createPullRequest: try await createPullRequest(arguments)
        case .reviewPullRequest: try await reviewPullRequest(arguments)
        case .mergePullRequest: try await mergePullRequest(arguments)
        }
    }
}

// MARK: Reading

extension GitHubCommands {
    private func issue(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/issues/\(arguments.number())"
        async let details = api.request(path: path)
        async let comments = api.list(path: path + "/comments", page: 1)
        var result = GitHubCommandSections(name: "issue", details: try await details.data)
        result.add("comments", try await comments, row: Self.commentRow)
        return result.value
    }

    private func pullRequest(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let base = try api.repositoryPath(arguments.repository()), number = try arguments.number()
        let pull = "\(base)/pulls/\(number)", issue = "\(base)/issues/\(number)"
        async let details = api.request(path: pull)
        async let comments = api.list(path: issue + "/comments", page: 1)
        async let reviews = api.list(path: pull + "/reviews", page: 1)
        async let reviewComments = api.list(path: pull + "/comments", page: 1)
        async let files = api.list(path: pull + "/files", page: 1)
        let pullRequest = try await details.data
        var result = GitHubCommandSections(name: "pull_request", details: pullRequest)
        result.add("comments", try await comments, row: Self.commentRow)
        result.add("reviews", try await reviews, row: Self.reviewRow)
        result.add("review_comments", try await reviewComments, row: Self.reviewCommentRow)
        result.add("files", try await files, row: Self.fileRow)
        try await addChecks(to: &result, commit: "\(base)/commits/\(pullRequest["head"]["sha"].text)")
        return result.value
    }

    /// Checks and commit statuses, which a connection without access to them leaves out instead of
    /// failing the whole pull request.
    private func addChecks(to result: inout GitHubCommandSections, commit: String) async throws {
        do {
            result.add("checks", try await api.list(path: commit + "/check-runs", page: 1, envelope: "check_runs"), row: Self.checkRow)
            result.add("statuses", try await api.list(path: commit + "/status", page: 1, envelope: "statuses"), row: Self.statusRow)
        } catch GitHubAPIError.forbidden, GitHubAPIError.notFound {
            result.note("checks_unavailable", "The GitHub connection cannot read this repository's checks.")
        }
    }

    private func pullRequestDiff(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/pulls/\(arguments.number())"
        let response = try await api.request(path: path, accept: "application/vnd.github.v3.diff", decodeJSON: false)
        guard let diff = response.data.string else { throw GitHubAPIError.invalidResponse }
        guard diff.utf8.count > Self.maximumDiffBytes else { return .object(["diff": .string(diff)]) }
        let shown = GitHubService.boundedUTF8(diff, maximumBytes: Self.maximumDiffBytes) + Self.shortenedMark
        return .object(["diff": .string(shown), "truncated": .bool(true)])
    }
}

// MARK: Writing

extension GitHubCommands {
    private func comment(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/issues/\(arguments.number())/comments"
        let body: GitHubValue = .object(["body": .string(try arguments.requiredText("body"))])
        let created = try await api.request(path: path, method: "POST", body: body, expectedStatuses: [201]).data
        return try Self.confirmed(created, by: "id", keeping: ["id", "html_url"])
    }

    private func createIssue(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/issues"
        var body: [String: GitHubValue] = ["title": .string(try arguments.requiredText("title", maximumBytes: 1024))]
        body["body"] = try arguments.text("body").map(GitHubValue.string)
        body["labels"] = try arguments.names("labels")
        body["assignees"] = try arguments.names("assignees")
        let created = try await api.request(path: path, method: "POST", body: .object(body), expectedStatuses: [201]).data
        return try Self.confirmed(created, by: "number", keeping: ["number", "html_url"])
    }

    private func updateIssue(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/issues/\(arguments.number())"
        var body: [String: GitHubValue] = [:]
        body["title"] = try arguments.text("title", maximumBytes: 1024).map(GitHubValue.string)
        body["body"] = try arguments.text("body").map(GitHubValue.string)
        body["state"] = try arguments.choice("state", from: ["open", "closed"]).map(GitHubValue.string)
        body["state_reason"] = try arguments.choice("state_reason", from: ["completed", "not_planned", "reopened"]).map(GitHubValue.string)
        body["labels"] = try arguments.names("labels")
        body["assignees"] = try arguments.names("assignees")
        guard !body.isEmpty else { throw GitHubCommandError.invalidArgument("Give at least one thing to change.") }
        let updated = try await api.request(path: path, method: "PATCH", body: .object(body)).data
        return try Self.confirmed(updated, by: "number", keeping: ["number", "state", "html_url"])
    }

    private func createPullRequest(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/pulls"
        var body: [String: GitHubValue] = [
            "title": .string(try arguments.requiredText("title", maximumBytes: 1024)),
            "head": .string(try arguments.requiredText("head", maximumBytes: 512)),
            "base": .string(try arguments.requiredText("base", maximumBytes: 512)),
        ]
        body["body"] = try arguments.text("body").map(GitHubValue.string)
        body["draft"] = try arguments.flag("draft").map(GitHubValue.bool)
        let created = try await api.request(path: path, method: "POST", body: .object(body), expectedStatuses: [201]).data
        return try Self.confirmed(created, by: "number", keeping: ["number", "draft", "html_url"])
    }

    private func reviewPullRequest(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/pulls/\(arguments.number())/reviews"
        guard let event = try arguments.choice("event", from: ["approve", "request_changes", "comment"]) else {
            throw GitHubCommandError.invalidArgument("event must be approve, request_changes or comment.")
        }
        var body: [String: GitHubValue] = ["event": .string(event.uppercased())]
        // GitHub accepts an approval without a word, and nothing else.
        body["body"] = try (event == "approve" ? arguments.text("body") : arguments.requiredText("body")).map(GitHubValue.string)
        let review = try await api.request(path: path, method: "POST", body: .object(body)).data
        return try Self.confirmed(review, by: "id", keeping: ["id", "state", "html_url"])
    }

    /// Merges only the commit the agent names, so a pull request that moved since it was read is refused.
    private func mergePullRequest(_ arguments: GitHubCommandArguments) async throws -> GitHubValue {
        let path = try api.repositoryPath(arguments.repository()) + "/pulls/\(arguments.number())/merge"
        guard let method = try arguments.choice("method", from: GitHubMergeMethod.allCases.map(\.rawValue)) else {
            throw GitHubCommandError.invalidArgument("method must be merge, squash or rebase.")
        }
        let sha = try arguments.requiredText("sha", maximumBytes: 40).lowercased()
        guard GitHubService.isSHA(sha) else {
            throw GitHubCommandError.invalidArgument("sha must be the pull request's full head commit SHA.")
        }
        let merged = try await api.request(path: path, method: "PUT", body: .object(["sha": .string(sha), "merge_method": .string(method)])).data
        guard merged["merged"].bool == true else { throw GitHubAPIError.validationFailed }
        return merged.selecting(["merged", "sha", "message"])
    }
}

// MARK: Shaping results

extension GitHubCommands {
    static func page(_ page: GitHubPage, row: (GitHubValue) -> GitHubValue) -> GitHubValue {
        var result: [String: GitHubValue] = ["items": .array(page.items.map(row))]
        result["nextPage"] = page.nextPage.map { .number(Double($0)) }
        return .object(result)
    }

    /// A row of a list: enough to choose an item, without its body.
    static func itemSummary(_ row: GitHubValue) -> GitHubValue {
        var summary = row.selecting(["number", "title", "state", "draft", "user", "comments", "created_at", "updated_at", "html_url"]).compact().object
        let labels = row["labels"].array.compactMap { $0["name"].string }
        if !labels.isEmpty { summary["labels"] = .array(labels.map(GitHubValue.string)) }
        let assignees = row["assignees"].array.compactMap { $0["login"].string }
        if !assignees.isEmpty { summary["assignees"] = .array(assignees.map(GitHubValue.string)) }
        summary["head"] = row["head"]["ref"].string.map(GitHubValue.string)
        summary["base"] = row["base"]["ref"].string.map(GitHubValue.string)
        return .object(summary)
    }

    static func commentRow(_ row: GitHubValue) -> GitHubValue {
        row.selecting(["id", "user", "author_association", "body", "created_at", "updated_at", "html_url"]).compact()
    }

    static func reviewRow(_ row: GitHubValue) -> GitHubValue {
        row.selecting(["id", "user", "state", "body", "submitted_at", "html_url"]).compact()
    }

    static func reviewCommentRow(_ row: GitHubValue) -> GitHubValue {
        row.selecting(["id", "user", "path", "line", "start_line", "side", "body", "in_reply_to_id", "created_at", "html_url"]).compact()
    }

    /// A changed file without its patch; the diff command returns the changes themselves.
    static func fileRow(_ row: GitHubValue) -> GitHubValue {
        row.selecting(["filename", "previous_filename", "status", "additions", "deletions", "changes"])
    }

    static func checkRow(_ row: GitHubValue) -> GitHubValue {
        row.selecting(["name", "status", "conclusion", "details_url", "started_at", "completed_at"])
    }

    static func statusRow(_ row: GitHubValue) -> GitHubValue {
        row.selecting(["context", "state", "description", "target_url"])
    }

    /// What GitHub answered to a write, once the field that proves it happened is there.
    static func confirmed(_ value: GitHubValue, by key: String, keeping keys: [String]) throws -> GitHubValue {
        guard value[key].int != nil else { throw GitHubAPIError.invalidResponse }
        return value.selecting(keys)
    }

    /// The result within what one tool answer may hold, shortening long text before giving up.
    static func bounded(_ value: GitHubValue) throws -> GitHubValue {
        let textLimits: [Int?] = [nil, 16384, 4096, 1024]
        for limit in textLimits {
            let candidate = limit.map { value.shorteningText(maximumBytes: $0, mark: shortenedMark) } ?? value
            if try JSONEncoder().encode(candidate).count <= maximumResultBytes { return candidate }
        }
        throw GitHubCommandError.responseTooLarge
    }
}

/// An item with the lists that belong to it, and the names of the lists GitHub has more pages of.
struct GitHubCommandSections {
    private var fields: [String: GitHubValue]
    private var more: [String] = []

    init(name: String, details: GitHubValue) { fields = [name: details.compact()] }

    mutating func add(_ name: String, _ page: GitHubPage, row: (GitHubValue) -> GitHubValue) {
        fields[name] = .array(page.items.map(row))
        if page.nextPage != nil { more.append(name) }
    }

    mutating func note(_ name: String, _ text: String) { fields[name] = .string(text) }

    var value: GitHubValue {
        var result = fields
        if !more.isEmpty { result["more"] = .array(more.map(GitHubValue.string)) }
        return .object(result)
    }
}
