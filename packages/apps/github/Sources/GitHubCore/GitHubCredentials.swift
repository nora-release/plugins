import Foundation

public struct GitHubCredentials: Codable, Sendable, Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    public let token: String
    public let expiresAt: Date?
    public let refreshToken: String?
    public let refreshExpiresAt: Date?
    public init(token: String, expiresAt: Date? = nil, refreshToken: String? = nil, refreshExpiresAt: Date? = nil) {
        self.token = token; self.expiresAt = expiresAt; self.refreshToken = refreshToken; self.refreshExpiresAt = refreshExpiresAt
    }
    private enum CodingKeys: String, CodingKey { case token, expiresAt, refreshToken, refreshExpiresAt }
    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        token = try values.decode(String.self, forKey: .token)
        refreshToken = try values.decodeIfPresent(String.self, forKey: .refreshToken)
        func date(_ key: CodingKeys) throws -> Date? {
            guard let milliseconds = try values.decodeIfPresent(Double.self, forKey: key) else { return nil }
            guard milliseconds.isFinite, milliseconds >= 0 else { throw GitHubAPIError.invalidCredential }
            return Date(timeIntervalSince1970: milliseconds / 1000)
        }
        expiresAt = try date(.expiresAt)
        refreshExpiresAt = try date(.refreshExpiresAt)
    }
    public func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(token, forKey: .token)
        try values.encodeIfPresent(refreshToken, forKey: .refreshToken)
        try values.encodeIfPresent(expiresAt.map { $0.timeIntervalSince1970 * 1000 }, forKey: .expiresAt)
        try values.encodeIfPresent(refreshExpiresAt.map { $0.timeIntervalSince1970 * 1000 }, forKey: .refreshExpiresAt)
    }
    public func isExpired(at date: Date = Date()) -> Bool { expiresAt.map { $0 <= date } ?? false }
    public var description: String { "GitHubCredentials(<redacted>)" }
    public var debugDescription: String { description }
    public static func isValidToken(_ token: String) -> Bool {
        !token.isEmpty && token.utf8.count <= 4096 && token.utf8.allSatisfy { (33...126).contains($0) }
    }
    public var isValid: Bool {
        Self.isValidToken(token) && (refreshToken.map(Self.isValidToken) ?? true)
            && (expiresAt?.timeIntervalSinceReferenceDate.isFinite ?? true)
            && (refreshExpiresAt?.timeIntervalSinceReferenceDate.isFinite ?? true)
            && (refreshExpiresAt == nil || refreshToken != nil)
    }
}

public protocol GitHubCredentialStore: Sendable {
    func load() async throws -> GitHubCredentials?
    func save(_ credentials: GitHubCredentials) async throws
    func remove() async throws
}

public struct GitHubHostCredentialStore: GitHubCredentialStore {
    private let request: GitHubHostRequest
    public init(request: @escaping GitHubHostRequest) { self.request = request }
    public func load() async throws -> GitHubCredentials? {
        let value = try await request("credentials.get", .object(["key": .string("connection")]))
        guard !value.isNull else { return nil }
        guard let text = value.string, text.utf8.count <= 16_384,
              let saved = try? JSONDecoder().decode(GitHubCredentials.self, from: Data(text.utf8)), saved.isValid else {
            throw GitHubAPIError.invalidCredential
        }
        return saved
    }
    public func save(_ credentials: GitHubCredentials) async throws {
        guard credentials.isValid else { throw GitHubAPIError.invalidCredential }
        let text = String(decoding: try JSONEncoder().encode(credentials), as: UTF8.self)
        _ = try await request("credentials.set", .object(["key": .string("connection"), "value": .string(text)]))
    }
    public func remove() async throws { _ = try await request("credentials.delete", .object(["key": .string("connection")])) }
}
