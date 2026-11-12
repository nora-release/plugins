import Foundation
import Observation

@Observable @MainActor
public final class GitHubService {
    public internal(set) var user: GitHubValue?
    public internal(set) var items: [GitHubValue] = []
    public internal(set) var selectedItem: GitHubValue?
    public internal(set) var detail: GitHubValue?
    public internal(set) var sections: [GitHubSection: GitHubSectionState] = [:]
    /// The kind of the item whose details are open.
    public internal(set) var kind: GitHubKind = .issues
    public var query = ""
    public internal(set) var repositories: [GitHubValue] = []
    public internal(set) var isLoadingFeed = false
    public internal(set) var isLoadingDetail = false
    public internal(set) var isConnecting = false
    public internal(set) var restored = false
    public internal(set) var isReady = false
    public internal(set) var error: String?
    public internal(set) var detailError: String?
    public internal(set) var deviceAuthorization: GitHubDeviceAuthorization?
    public internal(set) var moreFeed = false
    public internal(set) var feedNotice: String?
    public internal(set) var mergeProposal: GitHubMergeProposal?
    public internal(set) var mergeError: String?
    public internal(set) var commentError: String?
    public internal(set) var commentNotice: String?
    public internal(set) var isPostingComment = false
    public internal(set) var isMerging = false
    public var onPresentationChange: (@MainActor () async -> Void)?
    public var onUIAction: (@MainActor (String) -> Bool)?
    @ObservationIgnored private let credentialCoordinator: GitHubCredentialCoordinator
    @ObservationIgnored private var host: GitHubHostRequest?
    @ObservationIgnored private var session: GitHubSession?
    @ObservationIgnored private var transport: (any GitHubHTTPTransport)?
    @ObservationIgnored var cachedDiff: (String, String)?
    @ObservationIgnored var uncertainComments = Set<String>()
    /// The next search page of each kind, nil once it has no more.
    @ObservationIgnored private var nextFeedPages: [GitHubKind: Int] = [:]
    @ObservationIgnored private var epoch = 0
    @ObservationIgnored private var feedRevision = 0
    @ObservationIgnored private var detailRevision = 0
    @ObservationIgnored private var managedRepositories: [String]?
    @ObservationIgnored private var loginTask: Task<Void, Never>?

    public init(credentialCoordinator: GitHubCredentialCoordinator = GitHubCredentialCoordinator()) { self.credentialCoordinator = credentialCoordinator }
    public init(request: @escaping GitHubHostRequest, credentialCoordinator: GitHubCredentialCoordinator = GitHubCredentialCoordinator()) {
        self.credentialCoordinator = credentialCoordinator
        configure(request: request)
    }
    public func configure(request: @escaping GitHubHostRequest) {
        host = request
        isReady = true
        guard session == nil else { return }
        let forwarding: GitHubHostRequest = { [weak self] method, parameters in
            guard let self else { throw GitHubAPIError.unauthenticated }
            return try await self.requestHost(method, parameters)
        }
        let transport = GitHubHostTransport(request: forwarding)
        self.transport = transport
        session = GitHubSession(store: GitHubHostCredentialStore(request: forwarding), transport: transport, coordinator: credentialCoordinator)
    }
    public func activate() async {
        isReady = true
        await present()
        await restore()
    }
    public func restore() async {
        guard isReady, !restored, let session else { return }
        restored = true
        do {
            let api = try await session.api()
            let current = epoch
            let value = try await api.currentUser()
            guard epoch == current else { return }
            try validateUser(value)
            user = value
            await refreshFeed()
        } catch is CancellationError {} catch GitHubAPIError.unauthenticated {} catch { self.error = error.localizedDescription }
    }
    public func connect() async {
        guard !isConnecting, let transport else { return }
        epoch += 1; feedRevision += 1; loginTask?.cancel(); isLoadingFeed = false
        isConnecting = true; error = nil
        let current = epoch
        do {
            let login = GitHubDeviceLogin(clientID: GitHubSession.clientID, transport: transport)
            let authorization = try await login.begin()
            guard epoch == current else { return }
            deviceAuthorization = authorization
            try await openURL(authorization.verificationURL)
            loginTask?.cancel()
            loginTask = Task { [weak self] in
                do {
                    let credentials = try await login.wait(for: authorization)
                    try Task.checkCancellation()
                    await self?.finishLogin(credentials, expectedEpoch: current)
                } catch is CancellationError {} catch { self?.error = error.localizedDescription }
                if self?.epoch == current { self?.isConnecting = false; self?.deviceAuthorization = nil }
            }
        } catch { isConnecting = false; self.error = error.localizedDescription }
    }
    public func connectToken(_ token: String) async {
        guard GitHubCredentials.isValidToken(token) else { error = GitHubAPIError.invalidCredential.localizedDescription; return }
        epoch += 1; feedRevision += 1; loginTask?.cancel(); isLoadingFeed = false
        isConnecting = true
        await finishLogin(GitHubCredentials(token: token), expectedEpoch: epoch)
        isConnecting = false
    }
    private func finishLogin(_ credentials: GitHubCredentials, expectedEpoch: Int) async {
        guard let transport, let session else { return }
        do {
            let value = try await GitHubAPI(token: credentials.token, transport: transport).currentUser()
            try validateUser(value)
            guard epoch == expectedEpoch else { return }
            try await session.replace(credentials)
            guard epoch == expectedEpoch else { return }
            closeDetails(); items = []; repositories = []; moreFeed = false
            user = value; error = nil; managedRepositories = nil; nextFeedPages = [:]
            await refreshFeed()
        } catch is CancellationError {} catch { self.error = error.localizedDescription }
    }
    public func disconnect() async {
        epoch += 1; feedRevision += 1; loginTask?.cancel(); loginTask = nil
        user = nil; items = []; repositories = []; moreFeed = false; feedNotice = nil; managedRepositories = nil; nextFeedPages = [:]
        deviceAuthorization = nil; isConnecting = false; isLoadingFeed = false
        closeDetails()
        do { try await session?.replace(nil) } catch { self.error = error.localizedDescription }
        await present()
    }
    public func apiForCommands() async throws -> GitHubAPI { try await api() }
    public func refresh() async {
        if let item = selectedItem { await selectItem(item) }
        else { managedRepositories = nil; await refreshFeed() }
    }
    public func refreshFeed() async {
        feedRevision += 1
        isLoadingFeed = false
        await loadFeed(append: false)
    }
    public func loadMoreFeed() async { guard moreFeed else { return }; await loadFeed(append: true) }
    /// The open issues and pull requests related to the user, both kinds at once, a page of each
    /// at a time.
    private func loadFeed(append: Bool) async {
        guard !isLoadingFeed, let user, let session else { return }
        let revision = feedRevision, current = epoch
        let pages: [GitHubKind: Int] = append ? nextFeedPages : [.issues: 1, .pullRequests: 1]
        guard !pages.isEmpty else { return }
        isLoadingFeed = true; error = nil
        defer { if revision == feedRevision { isLoadingFeed = false } }
        do {
            let api = try await session.api()
            let login = user["login"].text
            @Sendable func search(_ kind: GitHubKind, managed: [String]) async throws -> GitHubPage? {
                guard let page = pages[kind] else { return nil }
                return try await api.relatedItems(login: login, kind: kind, managedRepositories: managed, page: page)
            }
            // Issues need no list of repositories, so they search while it loads; on an empty board
            // they show before the pull requests.
            async let managed = pages[.pullRequests] == nil ? [] : managedRepositoryNames(api: api, login: login, revision: revision, epoch: current)
            let issues = try await search(.issues, managed: [])
            guard epoch == current, revision == feedRevision else { return }
            if items.isEmpty, let issues { items = Self.unique(issues.items) }
            let pullRequests = try await search(.pullRequests, managed: try await managed)
            let results: [(GitHubKind, GitHubPage?)] = [(.issues, issues), (.pullRequests, pullRequests)]
            try Task.checkCancellation()
            guard epoch == current, revision == feedRevision else { return }
            var loaded: [GitHubValue] = [], next: [GitHubKind: Int] = [:], truncated = false
            for case let (kind, result?) in results {
                loaded += result.items
                if let following = result.nextPage { next[kind] = following }
                truncated = truncated || result.isTruncated
            }
            items = Self.unique((append ? items : []) + loaded)
            nextFeedPages = next; moreFeed = !next.isEmpty
            feedNotice = truncated ? GHC("GitHub search returns at most 1,000 items, so the oldest are left out.") : nil
        } catch is CancellationError {} catch { if revision == feedRevision { self.error = error.localizedDescription } }
    }
    /// The repositories the user maintains outside their own account, whose pull requests count as theirs.
    private func managedRepositoryNames(api: GitHubAPI, login: String, revision: Int, epoch current: Int) async throws -> [String] {
        if let managedRepositories { return managedRepositories }
        var next: Int? = 1, result: [String] = [], loadedRepos: [GitHubValue] = []
        while let page = next {
            let batch = try await api.repositories(page: page)
            try Task.checkCancellation()
            guard epoch == current, revision == feedRevision else { throw CancellationError() }
            loadedRepos += batch.items.filter { $0["archived"].bool != true }
            result += batch.items.filter { Self.isManagedRepository($0, login: login) }.map { $0["full_name"].text }
            next = batch.nextPage
        }
        let managed = Array(Set(result)).sorted()
        managedRepositories = managed; repositories = loadedRepos
        return managed
    }
    func removeFeedItem(key: String) {
        items.removeAll { Self.itemKey($0) == key }
    }
    public nonisolated static func isManagedRepository(_ repo: GitHubValue, login: String) -> Bool {
        guard repo["archived"].bool != true, GitHubAPI.isValidRepository(repo["full_name"].text) else { return false }
        guard repo["full_name"].text.split(separator: "/").first?.lowercased() != login.lowercased() else { return false }
        return repo["permissions"]["admin"].bool == true || repo["permissions"]["maintain"].bool == true
    }
    public nonisolated static func itemRepository(_ item: GitHubValue) -> String {
        if GitHubAPI.isValidRepository(item["repository"]["full_name"].text) { return item["repository"]["full_name"].text }
        let prefix = "https://api.github.com/repos/"
        let value = item["repository_url"].text
        return value.hasPrefix(prefix) ? String(value.dropFirst(prefix.count)) : ""
    }
    public nonisolated static func itemKey(_ item: GitHubValue) -> String { itemRepository(item).lowercased() + "/" + String(item["number"].int ?? 0) }
    public nonisolated static func unique(_ values: [GitHubValue]) -> [GitHubValue] {
        var seen = Set<String>()
        return values.filter { seen.insert(itemKey($0)).inserted }
    }
    public func closeDetails() {
        cachedDiff = nil; detailRevision += 1; selectedItem = nil; detail = nil; sections = [:]; detailError = nil
        isLoadingDetail = false; mergeProposal = nil; mergeError = nil; commentError = nil; commentNotice = nil
    }
    @discardableResult
    public func performPanelAction(_ action: String) async -> Bool {
        switch action {
        case "refresh": await refresh(); return true
        case "handoff": guard detail != nil else { return false }; await handToNora(); return true
        case "closeDetails":
            if onUIAction?(action) == true { return true }
            guard selectedItem != nil else { return false }
            closeDetails(); await present(); return true
        default: return onUIAction?(action) ?? false
        }
    }
    public func openURL(_ url: URL) async throws {
        guard let host, url.scheme == "https", url.user == nil, url.password == nil else { throw GitHubAPIError.invalidResponse }
        _ = try await host("browser.open", .object(["url": .string(url.absoluteString)]))
    }
    public func manageAccess() async { do { try await openURL(URL(string: "https://github.com/apps/nora-github-plugin/installations/new")!) } catch { self.error = error.localizedDescription } }
    public func handToNora() async {
        guard let detail, let host else { return }
        let repo = Self.itemRepository(detail), number = detail["number"].int ?? 0
        var text = "GitHub \(repo) #\(number): \(detail["title"].text)\n\(detail["html_url"].text)\n\n\(detail["body"].text)"
        for section in GitHubSection.allCases where section != .repository {
            let rows = sections[section]?.items ?? []
            if !rows.isEmpty {
                text += "\n\n\(section.rawValue):\n" + rows.map { $0["body"].string ?? $0["name"].string ?? $0["commit"]["message"].string ?? $0["event"].text }.joined(separator: "\n")
            }
        }
        do { _ = try await host("chat.openDraft", .object(["title": .string(Self.boundedUTF8(detail["title"].text, maximumBytes: 256)), "prompt": .string("Review this GitHub item and help complete the requested work."), "context": .string(Self.boundedUTF8(text, maximumBytes: 128 * 1024)), "sourceURL": detail["html_url"]])) }
        catch { self.error = error.localizedDescription }
    }
    nonisolated static func boundedUTF8(_ value: String, maximumBytes: Int) -> String {
        var bytes = Array(value.utf8.prefix(maximumBytes))
        while !bytes.isEmpty {
            if let result = String(bytes: bytes, encoding: .utf8) { return result }
            bytes.removeLast()
        }
        return ""
    }
    private func requestHost(_ method: String, _ parameters: GitHubValue) async throws -> GitHubValue {
        guard let host else { throw GitHubAPIError.unauthenticated }
        return try await host(method, parameters)
    }
    func present() async { await onPresentationChange?() }
    func validateUser(_ user: GitHubValue) throws {
        guard let login = user["login"].string, !login.isEmpty, login.utf8.count <= 100,
              login.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95 }) else { throw GitHubAPIError.invalidResponse }
    }
    func api() async throws -> GitHubAPI { guard let session else { throw GitHubAPIError.unauthenticated }; return try await session.api() }
    func currentDetailRevision() -> Int { detailRevision }
    func isCurrentDetail(_ revision: Int, key: String) -> Bool { detailRevision == revision && detail.map(Self.itemKey) == key }
}
