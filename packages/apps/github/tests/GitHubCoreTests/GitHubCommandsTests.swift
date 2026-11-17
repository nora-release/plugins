import Foundation
import Testing
@testable import GitHubCore

@Suite("GitHub commands for the agent")
struct GitHubCommandsTests {
    static let sha = String(repeating: "a", count: 40)

    @Test("The manifest lists every command, and marks the ones that change GitHub")
    func manifestMatchesCommands() throws {
        let manifestURL = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("nora-plugin.json")
        let manifest = try JSONDecoder().decode(GitHubValue.self, from: Data(contentsOf: manifestURL))
        let listed = Dictionary(uniqueKeysWithValues: manifest["commands"].array.map { ($0["id"].text, $0["readOnly"].bool == true) })
        let handled = Dictionary(uniqueKeysWithValues: GitHubCommand.allCases.map { ($0.id, $0.isReadOnly) })
        #expect(listed == handled)
    }

    @Test("A list row has what it takes to choose an item, and not its body")
    func listRowsAreSummaries() async throws {
        let transport = CommandTransport { _ in
            .array([Self.item(number: 7).setting("body", .string("A long description")).setting("labels", .array([.object(["name": .string("bug"), "color": .string("d73a4a")])]))])
        }
        let result = try await Self.commands(transport).execute(commandID: "com.nora.github.listIssues", arguments: .object(["repository": .string("nora-release/nora")]))
        let row = result["items"][0]
        #expect(row["number"].int == 7)
        #expect(row["user"].string == "mia")
        #expect(row["labels"] == .array([.string("bug")]))
        #expect(row["body"].isNull)
        #expect(result["nextPage"].isNull)
    }

    @Test("A pull request comes with its discussion, files and checks, and names the lists that have more")
    func pullRequestGathersSections() async throws {
        let transport = CommandTransport(nextPagePaths: ["/files"]) { request in
            let path = request.url!.path
            if path.hasSuffix("/pulls/5") { return Self.item(number: 5).setting("head", .object(["sha": .string(Self.sha), "ref": .string("fix")])) }
            if path.hasSuffix("/files") { return .array([.object(["filename": .string("a.swift"), "status": .string("modified"), "additions": .number(3), "patch": .string("@@ -1 +1 @@")])]) }
            if path.hasSuffix("/check-runs") { return .object(["check_runs": .array([.object(["name": .string("test"), "status": .string("completed"), "conclusion": .string("failure")])])]) }
            if path.hasSuffix("/status") { return .object(["state": .string("success"), "statuses": .array([])]) }
            if path.hasSuffix("/reviews") { return .array([.object(["id": .number(1), "state": .string("APPROVED"), "user": Self.account("jonas")])]) }
            return .array([])
        }
        let result = try await Self.commands(transport).execute(commandID: "com.nora.github.getPullRequest", arguments: Self.target(5))
        #expect(result["pull_request"]["head"]["ref"].string == "fix")
        #expect(result["files"][0]["filename"].string == "a.swift")
        #expect(result["files"][0]["patch"].isNull)
        #expect(result["checks"][0]["conclusion"].string == "failure")
        #expect(result["reviews"][0]["user"].string == "jonas")
        #expect(result["more"] == .array([.string("files")]))
        #expect(await transport.paths.contains("/repos/nora-release/nora/commits/\(Self.sha)/check-runs"))
    }

    @Test("A pull request whose checks the connection cannot read still comes back")
    func pullRequestWithoutChecks() async throws {
        let transport = CommandTransport(statuses: ["/check-runs": 403]) { request in
            request.url!.path.hasSuffix("/pulls/5") ? Self.item(number: 5).setting("head", .object(["sha": .string(Self.sha)])) : .array([])
        }
        let result = try await Self.commands(transport).execute(commandID: "com.nora.github.getPullRequest", arguments: Self.target(5))
        #expect(result["pull_request"]["number"].int == 5)
        #expect(result["checks"].isNull)
        #expect(result["checks_unavailable"].string != nil)
    }

    @Test("Answers lose API links and repeated accounts, and keep the object that was asked for")
    func compactAnswers() {
        let repository: GitHubValue = .object(["full_name": .string("nora-release/nora"), "owner": Self.account("nora-release"), "forks_url": .string("https://api.github.com/repos/nora-release/nora/forks"), "size": .number(9)])
        let value: GitHubValue = .object([
            "login": .string("mia"), "avatar_url": .string("https://avatars.githubusercontent.com/u/1"), "name": .string("Mia"),
            "url": .string("https://api.github.com/users/mia"), "html_url": .string("https://github.com/mia"), "email": .null,
            "details_url": .string("https://ci.example.com/run/1"), "assignee": Self.account("jonas"), "base": .object(["repo": repository]),
        ])
        let compact = value.compact()
        #expect(compact["name"].string == "Mia")
        #expect(compact.object.keys.sorted() == ["assignee", "base", "details_url", "html_url", "login", "name"])
        #expect(compact["assignee"].string == "jonas")
        #expect(compact["base"]["repo"] == .object(["full_name": .string("nora-release/nora")]))
        #expect(GitHubValue.array([Self.account("mia").setting("role_name", .string("admin"))]).compact()[0]["role_name"].string == "admin")
    }

    @Test("A comment is posted to the item, with the text as written")
    func comment() async throws {
        let transport = CommandTransport(status: 201) { _ in .object(["id": .number(9), "html_url": .string("https://github.com/nora-release/nora/issues/5#issuecomment-9"), "body": .string("Done")]) }
        let arguments = Self.target(5).setting("body", .string("Done\n"))
        let result = try await Self.commands(transport).execute(commandID: "com.nora.github.comment", arguments: arguments)
        let request = try #require(await transport.requests.first)
        #expect(request.httpMethod == "POST")
        #expect(request.url?.path == "/repos/nora-release/nora/issues/5/comments")
        #expect(try JSONDecoder().decode(GitHubValue.self, from: #require(request.httpBody)) == .object(["body": .string("Done\n")]))
        #expect(result == .object(["id": .number(9), "html_url": .string("https://github.com/nora-release/nora/issues/5#issuecomment-9")]))
    }

    @Test("A pull request is opened from the branch the agent names")
    func createPullRequest() async throws {
        let transport = CommandTransport(status: 201) { _ in .object(["number": .number(12), "html_url": .string("https://github.com/nora-release/nora/pull/12"), "draft": .bool(true)]) }
        let arguments: GitHubValue = .object(["repository": .string("nora-release/nora"), "title": .string("Fix login"), "head": .string("nora/fix-login"), "base": .string("main"), "draft": .bool(true)])
        let result = try await Self.commands(transport).execute(commandID: "com.nora.github.createPullRequest", arguments: arguments)
        let request = try #require(await transport.requests.first)
        #expect(request.url?.path == "/repos/nora-release/nora/pulls")
        let body = try JSONDecoder().decode(GitHubValue.self, from: #require(request.httpBody))
        #expect(body == .object(["title": .string("Fix login"), "head": .string("nora/fix-login"), "base": .string("main"), "draft": .bool(true)]))
        #expect(result["number"].int == 12)
    }

    @Test("A merge names the commit it merges, so a pull request that moved is refused")
    func mergeNamesItsCommit() async throws {
        let transport = CommandTransport { _ in .object(["merged": .bool(true), "sha": .string(Self.sha)]) }
        let commands = Self.commands(transport)
        await #expect(throws: GitHubCommandError.self) {
            _ = try await commands.execute(commandID: "com.nora.github.mergePullRequest", arguments: Self.target(5).setting("method", .string("squash")))
        }
        #expect(await transport.requests.isEmpty)
        let arguments = Self.target(5).setting("method", .string("squash")).setting("sha", .string(Self.sha))
        let result = try await commands.execute(commandID: "com.nora.github.mergePullRequest", arguments: arguments)
        let request = try #require(await transport.requests.first)
        #expect(request.httpMethod == "PUT")
        #expect(try JSONDecoder().decode(GitHubValue.self, from: #require(request.httpBody)) == .object(["sha": .string(Self.sha), "merge_method": .string("squash")]))
        #expect(result["merged"].bool == true)
    }

    @Test("A review other than an approval needs words")
    func reviewNeedsWords() async throws {
        let transport = CommandTransport { _ in .object(["id": .number(3), "state": .string("APPROVED")]) }
        let commands = Self.commands(transport)
        await #expect(throws: GitHubCommandError.invalidArgument("body is required.")) {
            _ = try await commands.execute(commandID: "com.nora.github.reviewPullRequest", arguments: Self.target(5).setting("event", .string("request_changes")))
        }
        _ = try await commands.execute(commandID: "com.nora.github.reviewPullRequest", arguments: Self.target(5).setting("event", .string("approve")))
        let request = try #require(await transport.requests.first)
        #expect(try JSONDecoder().decode(GitHubValue.self, from: #require(request.httpBody)) == .object(["event": .string("APPROVE")]))
    }

    @Test("An update with nothing to change sends nothing")
    func emptyUpdate() async {
        let transport = CommandTransport { _ in .null }
        await #expect(throws: GitHubCommandError.self) {
            _ = try await Self.commands(transport).execute(commandID: "com.nora.github.updateIssue", arguments: Self.target(5))
        }
        #expect(await transport.requests.isEmpty)
    }

    @Test("Any REST path can be read, and nothing that is not one", arguments: ["repos/a/b", "//evil.test/x", "https://evil.test/x", "/repos/a/b\n/x", "/repos/a/b#x", ""])
    func readRejectsOtherPaths(_ path: String) async {
        let transport = CommandTransport { _ in .null }
        await #expect(throws: GitHubCommandError.self) {
            _ = try await Self.commands(transport).execute(commandID: "com.nora.github.get", arguments: .object(["path": .string(path)]))
        }
        #expect(await transport.requests.isEmpty)
    }

    @Test("A read keeps the query the agent wrote")
    func readKeepsQuery() async throws {
        let transport = CommandTransport { _ in .object(["login": .string("mia"), "avatar_url": .string("x"), "name": .string("Mia")]) }
        let result = try await Self.commands(transport).execute(commandID: "com.nora.github.get", arguments: .object(["path": .string("/repos/a/b/releases?per_page=5")]))
        #expect(await transport.requests.first?.url?.absoluteString == "https://api.github.com/repos/a/b/releases?per_page=5")
        #expect(result["name"].string == "Mia")
        _ = try await Self.commands(transport).execute(commandID: "com.nora.github.get", arguments: .object(["path": .string("/search/issues?q=is:issue is:open involves:@me&per_page=30")]))
        #expect(await transport.requests.last?.url?.absoluteString == "https://api.github.com/search/issues?q=is:issue%20is:open%20involves:@me&per_page=30")
    }

    @Test("A long diff and long text are shortened to fit one answer, and say so")
    func longAnswersAreShortened() async throws {
        let long = String(repeating: "x", count: 400_000)
        let diff = try await Self.commands(CommandTransport(text: long) { _ in .null }).execute(commandID: "com.nora.github.getPullRequestDiff", arguments: Self.target(5))
        #expect(diff["truncated"].bool == true)
        #expect(diff["diff"].text.hasSuffix(GitHubCommands.shortenedMark))
        let issue = try await Self.commands(CommandTransport { request in
            request.url!.path.hasSuffix("/comments") ? .array([]) : Self.item(number: 5).setting("body", .string(long))
        }).execute(commandID: "com.nora.github.getIssue", arguments: Self.target(5))
        #expect(issue["issue"]["body"].text.utf8.count < 20_000)
        #expect(issue["issue"]["body"].text.hasSuffix(GitHubCommands.shortenedMark))
        #expect(try JSONEncoder().encode(issue).count <= GitHubCommands.maximumResultBytes)
    }

    @Test("An unknown command and a repository that is not one reach nothing")
    func invalidInput() async {
        let transport = CommandTransport { _ in .null }
        let commands = Self.commands(transport)
        await #expect(throws: GitHubCommandError.unknownCommand("com.nora.github.deleteRepository")) {
            _ = try await commands.execute(commandID: "com.nora.github.deleteRepository", arguments: .object([:]))
        }
        await #expect(throws: GitHubAPIError.invalidRepository) {
            _ = try await commands.execute(commandID: "com.nora.github.getIssue", arguments: .object(["repository": .string("../x"), "number": .number(1)]))
        }
        await #expect(throws: GitHubCommandError.self) {
            _ = try await commands.execute(commandID: "com.nora.github.listIssues", arguments: .object(["repository": .string("a/b"), "page": .string("2")]))
        }
        #expect(await transport.requests.isEmpty)
    }

    private static func commands(_ transport: CommandTransport) -> GitHubCommands { GitHubCommands(api: GitHubAPI(token: "test", transport: transport)) }
    private static func target(_ number: Int) -> GitHubValue { .object(["repository": .string("nora-release/nora"), "number": .number(Double(number))]) }
    private static func account(_ login: String) -> GitHubValue {
        .object(["login": .string(login), "id": .number(1), "avatar_url": .string("https://avatars.githubusercontent.com/u/1"), "url": .string("https://api.github.com/users/\(login)")])
    }
    private static func item(number: Int) -> GitHubValue {
        .object(["number": .number(Double(number)), "title": .string("Login fails"), "state": .string("open"), "user": account("mia"),
                 "html_url": .string("https://github.com/nora-release/nora/issues/\(number)"), "url": .string("https://api.github.com/repos/nora-release/nora/issues/\(number)")])
    }
}

/// Answers each request by its path, as GitHub would, and keeps what was asked.
actor CommandTransport: GitHubHTTPTransport {
    private(set) var requests: [URLRequest] = []
    private let status: Int
    private let statuses: [String: Int]
    private let nextPagePaths: [String]
    private let text: String?
    private let respond: @Sendable (URLRequest) -> GitHubValue

    init(status: Int = 200, statuses: [String: Int] = [:], nextPagePaths: [String] = [], text: String? = nil, respond: @escaping @Sendable (URLRequest) -> GitHubValue) {
        self.status = status; self.statuses = statuses; self.nextPagePaths = nextPagePaths; self.text = text; self.respond = respond
    }

    var paths: [String] { requests.compactMap { $0.url?.path } }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        requests.append(request)
        let path = request.url!.path
        let code = statuses.first { path.hasSuffix($0.key) }?.value ?? status
        let headers = nextPagePaths.contains(where: path.hasSuffix) ? ["Link": "<https://api.github.com/next>; rel=\"next\""] : [:]
        let data = try text.map { Data($0.utf8) } ?? JSONEncoder().encode(respond(request))
        return (data, HTTPURLResponse(url: request.url!, statusCode: code, httpVersion: nil, headerFields: headers)!)
    }
}
