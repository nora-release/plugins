import Foundation

public enum GitHubAPIError: Error, LocalizedError, Sendable, Equatable {
    case invalidCredential
    case searchScopeTooLarge
    case invalidRepository
    case invalidPage
    case unauthenticated
    case forbidden
    case ssoAuthorizationRequired
    case rateLimited(retryAt: Date?)
    case notFound
    case serverUnavailable
    case unexpectedStatus(Int)
    case invalidResponse
    case conflict
    case validationFailed
    case networkUnavailable

    public var errorDescription: String? {
        switch self {
        case .invalidCredential:
            GHC("Enter a valid GitHub token.")
        case .searchScopeTooLarge:
            GHC("Your repository scope is too large for one GitHub search. Select a specific repository.")
        case .invalidRepository:
            GHC("Choose a repository in owner/name format.")
        case .invalidPage:
            GHC("The requested GitHub page is invalid.")
        case .unauthenticated:
            GHC("Your GitHub connection has expired. Connect again.")
        case .forbidden:
            GHC("GitHub denied access. Check that your connection has read access to this repository.")
        case .ssoAuthorizationRequired:
            GHC("This organization requires SSO authorization. Authorize your GitHub connection for the organization and try again.")
        case .rateLimited:
            GHC("GitHub's request limit was reached. Wait a moment and try again.")
        case .notFound:
            GHC("This repository is unavailable or your GitHub connection cannot access it.")
        case .serverUnavailable:
            GHC("GitHub is temporarily unavailable. Try again later.")
        case .unexpectedStatus:
            GHC("GitHub could not complete this request. Try again.")
        case .invalidResponse:
            GHC("GitHub returned an unexpected response. Try again.")
        case .conflict:
            GHC("The PR changed. Refresh and confirm the latest commit before merging.")
        case .validationFailed:
            GHC("GitHub rejected the request. Check its content and repository rules.")
        case .networkUnavailable:
            GHC("Could not reach GitHub. Check your internet connection and try again.")
        }
    }
}

public struct GitHubAPI: Sendable {
    public static let apiVersion = "2026-03-10"
    private static let pageLimit = 10_000
    private let token: String
    private let transport: any GitHubHTTPTransport
    public init(token: String, transport: any GitHubHTTPTransport) {
        self.token = token; self.transport = transport
    }
    public func request(path: String, method: String = "GET", body: GitHubValue? = nil, expectedStatuses: Set<Int> = [200], accept: String = "application/vnd.github+json", decodeJSON: Bool = true) async throws -> GitHubResult {
        try Task.checkCancellation()
        guard GitHubCredentials.isValidToken(token), path.hasPrefix("/"), !path.hasPrefix("//"),
              let url = URL(string: "https://api.github.com" + path), url.host == "api.github.com", url.user == nil, url.password == nil else {
            throw GitHubAPIError.invalidResponse
        }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 30)
        request.httpMethod = method
        request.setValue(accept, forHTTPHeaderField: "Accept")
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(Self.apiVersion, forHTTPHeaderField: "X-GitHub-Api-Version")
        if let body {
            request.httpBody = try JSONEncoder().encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let data: Data
        let response: HTTPURLResponse
        do { (data, response) = try await transport.send(request) }
        catch is CancellationError { throw CancellationError() }
        catch let error as URLError where error.code == .cancelled { throw CancellationError() }
        catch { throw GitHubAPIError.networkUnavailable }
        try Task.checkCancellation()
        guard response.url == request.url else { throw GitHubAPIError.invalidResponse }
        guard expectedStatuses.contains(response.statusCode) else { throw responseError(response, data: data) }
        let value = data.isEmpty ? GitHubValue.null : decodeJSON ? try decode(GitHubValue.self, from: data) : .string(String(decoding: data, as: UTF8.self))
        return GitHubResult(data: value, headers: response.allHeaderFields.reduce(into: [:]) { result, field in
            if let key = field.key as? String, let value = field.value as? String { result[key.lowercased()] = value }
        }, status: response.statusCode)
    }
    public func currentUser() async throws -> GitHubValue { try await request(path: "/user").data }
    public func repositories(page: Int = 1) async throws -> GitHubPage {
        try await list(path: "/user/repos?sort=updated&affiliation=owner,collaborator,organization_member", page: page)
    }
    public func issues(repository: String, state: GitHubItemState = .open, page: Int = 1) async throws -> GitHubPage {
        let result = try await list(path: repositoryPath(repository) + "/issues?state=\(state.rawValue)&sort=updated&direction=desc", page: page)
        return GitHubPage(items: result.items.filter { $0["pull_request"].isNull }, nextPage: result.nextPage)
    }
    public func pullRequests(repository: String, state: GitHubItemState = .open, page: Int = 1) async throws -> GitHubPage {
        try await list(path: repositoryPath(repository) + "/pulls?state=\(state.rawValue)&sort=updated&direction=desc", page: page)
    }
    public func list(path: String, page: Int, envelope: String? = nil) async throws -> GitHubPage {
        guard (1...Self.pageLimit).contains(page) else { throw GitHubAPIError.invalidPage }
        let result = try await request(path: path + (path.contains("?") ? "&" : "?") + "per_page=100&page=\(page)")
        let rows = envelope.map { result.data[$0] } ?? result.data
        guard case .array(let items) = rows else { throw GitHubAPIError.invalidResponse }
        return GitHubPage(items: items, nextPage: result.hasNext ? page + 1 : nil)
    }
    public func repositoryPath(_ name: String) throws -> String {
        guard Self.isValidRepository(name) else { throw GitHubAPIError.invalidRepository }
        return "/repos/" + name
    }
    public static func isValidRepository(_ repository: String) -> Bool {
        let parts = repository.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 2 else { return false }
        let owner = parts[0]
        let name = parts[1]
        let ownerCharacters = Set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-")
        let nameCharacters = ownerCharacters.union(Set("._"))
        guard !owner.isEmpty, owner.count <= 39,
              owner.first != "-", owner.last != "-", !owner.contains("--"),
              owner.allSatisfy({ ownerCharacters.contains($0) }),
              !name.isEmpty, name.count <= 100, name != ".", name != "..",
              name.allSatisfy({ nameCharacters.contains($0) })
        else { return false }
        return true
    }

    private func responseError(_ response: HTTPURLResponse, data: Data) -> GitHubAPIError {
        let status = response.statusCode
        if status == 401 { return .unauthenticated }
        if status == 403, response.value(forHTTPHeaderField: "X-GitHub-SSO")?.hasPrefix("required") == true {
            return .ssoAuthorizationRequired
        }
        let message = data.count <= 65_536
            ? (try? JSONDecoder().decode(ErrorEnvelope.self, from: data).message.lowercased())
            : nil
        if status == 429 || (status == 403 && (
            response.value(forHTTPHeaderField: "X-RateLimit-Remaining") == "0"
            || response.value(forHTTPHeaderField: "Retry-After") != nil
            || message?.contains("rate limit") == true
            || message?.contains("abuse detection") == true
        )) {
            let retryAt: Date?
            if let delay = response.value(forHTTPHeaderField: "Retry-After").flatMap(TimeInterval.init),
               delay.isFinite, delay >= 0, delay <= 86_400 {
                retryAt = Date().addingTimeInterval(delay)
            } else if let reset = response.value(forHTTPHeaderField: "X-RateLimit-Reset").flatMap(TimeInterval.init),
                      reset.isFinite, reset >= 0 {
                retryAt = Date(timeIntervalSince1970: reset)
            } else {
                retryAt = nil
            }
            return .rateLimited(retryAt: retryAt)
        }
        if status == 403 { return .forbidden }
        if status == 409 { return .conflict }
        if status == 422 { return .validationFailed }
        if status == 404 { return .notFound }
        if (500...599).contains(status) { return .serverUnavailable }
        return .unexpectedStatus(status)
    }

    private func decode<Value: Decodable>(_ type: Value.Type, from data: Data) throws -> Value {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            if let date = formatter.date(from: text) { return date }
            formatter.formatOptions = [.withInternetDateTime]
            guard let date = formatter.date(from: text) else { throw GitHubAPIError.invalidResponse }
            return date
        }
        do { return try decoder.decode(type, from: data) }
        catch { throw GitHubAPIError.invalidResponse }
    }


}

private struct ErrorEnvelope: Decodable {
    let message: String
}

public struct GitHubResult: Sendable {
    public let data: GitHubValue
    public let headers: [String: String]
    public let status: Int
    public var hasNext: Bool { headers["link"]?.contains("rel=\"next\"") == true }
}
