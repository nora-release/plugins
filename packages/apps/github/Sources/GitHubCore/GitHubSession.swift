import Foundation

public struct GitHubSession: Sendable {
    public static var clientID: String {
        let bundled = Bundle.main.object(forInfoDictionaryKey: "NoraGitHubClientID") as? String
        return bundled?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty ?? "Iv23liPZy7EARoSnYoYy"
    }
    private let store: any GitHubCredentialStore
    private let transport: any GitHubHTTPTransport
    private let coordinator: GitHubCredentialCoordinator
    public init(store: any GitHubCredentialStore, transport: any GitHubHTTPTransport,
                coordinator: GitHubCredentialCoordinator = GitHubCredentialCoordinator()) {
        self.store = store; self.transport = transport; self.coordinator = coordinator
    }
    public func replace(_ credentials: GitHubCredentials?) async throws { try await coordinator.replace(credentials, store: store) }
    public func activeCredentials() async throws -> GitHubCredentials { try await coordinator.activeCredentials(store: store, transport: transport) }
    public func api() async throws -> GitHubAPI { try await GitHubAPI(token: activeCredentials().token, transport: transport) }
}

/// Scenes share credential ordering while retaining their own authenticated host connections.
public actor GitHubCredentialCoordinator {
    private var epoch = 0
    private var credentialVersion = 0
    private var refresh: (UUID, Task<GitHubCredentials, any Error>)?
    private var persistence: Task<Void, any Error>?
    public init() {}

    public func replace(_ credentials: GitHubCredentials?, store: any GitHubCredentialStore) async throws {
        epoch += 1
        refresh = nil
        let previous = persistence
        let task = Task {
            _ = await previous?.result
            if let credentials { try await store.save(credentials) }
            else { try await store.remove() }
        }
        persistence = task
        try await task.value
    }
    public func activeCredentials(store: any GitHubCredentialStore, transport: any GitHubHTTPTransport) async throws -> GitHubCredentials {
        _ = await persistence?.result
        let current = epoch, version = credentialVersion
        let saved = try await store.load()
        guard current == epoch else { throw CancellationError() }
        if version != credentialVersion { return try await activeCredentials(store: store, transport: transport) }
        guard let credentials = saved, credentials.isValid else { throw GitHubAPIError.unauthenticated }
        guard let expiresAt = credentials.expiresAt, expiresAt.timeIntervalSinceNow <= 60 else { return credentials }
        guard credentials.refreshToken != nil else { throw GitHubAPIError.unauthenticated }
        if let refresh {
            let result = try await refresh.1.value
            guard epoch == current else { throw CancellationError() }
            return result
        }
        let id = UUID()
        let task = Task { try await GitHubDeviceLogin(clientID: GitHubSession.clientID, transport: transport).refresh(credentials) }
        refresh = (id, task)
        defer { if refresh?.0 == id { refresh = nil } }
        let refreshed = try await task.value
        guard epoch == current, try await store.load() == credentials else { throw CancellationError() }
        let previous = persistence
        let persistenceTask = Task {
            _ = await previous?.result
            try await store.save(refreshed)
        }
        persistence = persistenceTask
        try await persistenceTask.value
        credentialVersion += 1
        guard epoch == current else { throw CancellationError() }
        return refreshed
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
