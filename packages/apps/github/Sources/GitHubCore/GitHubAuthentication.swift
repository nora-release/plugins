import Foundation

public struct GitHubDeviceAuthorization: Sendable, Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    public let deviceCode: String
    public let userCode: String
    public let verificationURL: URL
    public let expiresAt: Date
    public let interval: TimeInterval

    public init(deviceCode: String, userCode: String, verificationURL: URL, expiresAt: Date, interval: TimeInterval) {
        self.deviceCode = deviceCode
        self.userCode = userCode
        self.verificationURL = verificationURL
        self.expiresAt = expiresAt
        self.interval = interval
    }

    public var description: String { "GitHubDeviceAuthorization(<redacted>)" }
    public var debugDescription: String { description }
}

public enum GitHubAuthenticationError: Error, LocalizedError, Sendable, Equatable {
    case invalidClientID
    case invalidResponse
    case accessDenied
    case authorizationExpired
    case deviceFlowDisabled
    case incorrectDeviceCode
    case refreshUnavailable
    case refreshExpired
    case rateLimited
    case serviceUnavailable
    case connectionFailed
    case requestRejected

    public var errorDescription: String? {
        switch self {
        case .invalidClientID:
            GHC("GitHub browser login is not configured with a valid Client ID.")
        case .invalidResponse:
            GHC("GitHub returned an invalid login response. Try connecting again.")
        case .accessDenied:
            GHC("GitHub authorization was declined.")
        case .authorizationExpired:
            GHC("The GitHub authorization code expired. Start connecting again.")
        case .deviceFlowDisabled:
            GHC("Device flow must be enabled in the GitHub App settings.")
        case .incorrectDeviceCode:
            GHC("GitHub could not verify this authorization code. Start connecting again.")
        case .refreshUnavailable, .refreshExpired:
            GHC("The GitHub connection expired. Connect again.")
        case .rateLimited:
            GHC("GitHub received too many login requests. Try again later.")
        case .serviceUnavailable:
            GHC("GitHub login is temporarily unavailable. Try again later.")
        case .connectionFailed:
            GHC("GitHub could not be reached. Check your connection and try again.")
        case .requestRejected:
            GHC("GitHub rejected the login request. Check the GitHub App configuration.")
        }
    }
}

public struct GitHubDeviceLogin: Sendable {
    private let clientID: String
    private let transport: any GitHubHTTPTransport
    private let now: @Sendable () -> Date
    private let sleep: @Sendable (TimeInterval) async throws -> Void

    public init(
        clientID: String,
        transport: any GitHubHTTPTransport,
        now: @escaping @Sendable () -> Date = { Date() },
        sleep: @escaping @Sendable (TimeInterval) async throws -> Void = {
            try await Task.sleep(for: .seconds($0))
        }
    ) {
        self.clientID = clientID.trimmingCharacters(in: .whitespacesAndNewlines)
        self.transport = transport
        self.now = now
        self.sleep = sleep
    }

    public func begin() async throws -> GitHubDeviceAuthorization {
        try validateClientID()
        let response: DeviceResponse = try await send(
            endpoint: "https://github.com/login/device/code",
            fields: [("client_id", clientID)]
        )
        if let error = response.error { throw authenticationError(error) }
        guard let deviceCode = response.deviceCode, Self.isValidDeviceCode(deviceCode),
              let userCode = response.userCode, Self.isValidUserCode(userCode),
              response.verificationURI == "https://github.com/login/device",
              let expiresIn = response.expiresIn, (1...86_400).contains(expiresIn),
              let interval = response.interval, (1...3600).contains(interval),
              let verificationURL = URL(string: "https://github.com/login/device") else {
            throw GitHubAuthenticationError.invalidResponse
        }
        return GitHubDeviceAuthorization(
            deviceCode: deviceCode,
            userCode: userCode,
            verificationURL: verificationURL,
            expiresAt: now().addingTimeInterval(TimeInterval(expiresIn)),
            interval: TimeInterval(interval)
        )
    }

    public func wait(for authorization: GitHubDeviceAuthorization) async throws -> GitHubCredentials {
        try validateClientID()
        guard Self.isValidDeviceCode(authorization.deviceCode),
              Self.isValidUserCode(authorization.userCode),
              authorization.verificationURL.absoluteString == "https://github.com/login/device",
              authorization.expiresAt.timeIntervalSinceReferenceDate.isFinite,
              (1...3600).contains(authorization.interval) else {
            throw GitHubAuthenticationError.invalidResponse
        }
        var interval = authorization.interval
        while true {
            try Task.checkCancellation()
            let remaining = authorization.expiresAt.timeIntervalSince(now())
            guard remaining > 0 else { throw GitHubAuthenticationError.authorizationExpired }
            try await sleep(min(interval, remaining))
            try Task.checkCancellation()
            guard now() < authorization.expiresAt else { throw GitHubAuthenticationError.authorizationExpired }
            let response: TokenResponse = try await send(
                endpoint: "https://github.com/login/oauth/access_token",
                fields: [
                    ("client_id", clientID),
                    ("device_code", authorization.deviceCode),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ]
            )
            if let error = response.error {
                switch error {
                case "authorization_pending":
                    continue
                case "slow_down":
                    interval += 5
                    if let reportedInterval = response.interval {
                        guard (1...3600).contains(reportedInterval) else {
                            throw GitHubAuthenticationError.invalidResponse
                        }
                        interval = max(interval, TimeInterval(reportedInterval))
                    }
                    continue
                default:
                    throw authenticationError(error)
                }
            }
            return try credentials(from: response)
        }
    }

    public func refresh(_ credentials: GitHubCredentials) async throws -> GitHubCredentials {
        try validateClientID()
        guard credentials.isValid, let refreshToken = credentials.refreshToken else {
            throw GitHubAuthenticationError.refreshUnavailable
        }
        if let expiresAt = credentials.refreshExpiresAt, expiresAt <= now() {
            throw GitHubAuthenticationError.refreshExpired
        }
        let response: TokenResponse = try await send(
            endpoint: "https://github.com/login/oauth/access_token",
            fields: [
                ("client_id", clientID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refreshToken),
            ]
        )
        if let error = response.error { throw authenticationError(error) }
        return try self.credentials(from: response)
    }

    private func credentials(from response: TokenResponse) throws -> GitHubCredentials {
        guard let token = response.accessToken, GitHubCredentials.isValidToken(token),
              response.tokenType?.lowercased() == "bearer" else {
            throw GitHubAuthenticationError.invalidResponse
        }
        let timestamp = now()
        let expiresAt = try expiration(response.expiresIn, from: timestamp)
        let refreshExpiresAt = try expiration(response.refreshTokenExpiresIn, from: timestamp)
        if let refreshToken = response.refreshToken {
            guard GitHubCredentials.isValidToken(refreshToken), refreshExpiresAt != nil else {
                throw GitHubAuthenticationError.invalidResponse
            }
        } else if refreshExpiresAt != nil {
            throw GitHubAuthenticationError.invalidResponse
        }
        return GitHubCredentials(
            token: token,
            expiresAt: expiresAt,
            refreshToken: response.refreshToken,
            refreshExpiresAt: refreshExpiresAt
        )
    }

    private func expiration(_ seconds: Int?, from timestamp: Date) throws -> Date? {
        guard let seconds else { return nil }
        guard (1...63_072_000).contains(seconds) else { throw GitHubAuthenticationError.invalidResponse }
        return timestamp.addingTimeInterval(TimeInterval(seconds))
    }

    private func validateClientID() throws {
        guard !clientID.isEmpty, clientID.utf8.count <= 256,
              clientID.utf8.allSatisfy({ Self.isASCIIAlphanumeric($0) || [45, 46, 95].contains($0) }) else {
            throw GitHubAuthenticationError.invalidClientID
        }
    }

    private func send<Response: Decodable>(endpoint: String, fields: [(String, String)]) async throws -> Response {
        try Task.checkCancellation()
        guard let url = URL(string: endpoint) else { throw GitHubAuthenticationError.invalidResponse }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 30
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("Nora-GitHub-Plugin", forHTTPHeaderField: "User-Agent")
        request.httpBody = Data(fields.map { "\(Self.formEncode($0.0))=\(Self.formEncode($0.1))" }.joined(separator: "&").utf8)
        let data: Data
        let response: HTTPURLResponse
        do {
            (data, response) = try await transport.send(request)
        } catch {
            if error is CancellationError || (error as? URLError)?.code == .cancelled {
                throw CancellationError()
            }
            throw GitHubAuthenticationError.connectionFailed
        }
        try Task.checkCancellation()
        guard response.url?.absoluteString == endpoint else { throw GitHubAuthenticationError.invalidResponse }
        switch response.statusCode {
        case 200...299:
            break
        case 429:
            throw GitHubAuthenticationError.rateLimited
        case 500...599:
            throw GitHubAuthenticationError.serviceUnavailable
        default:
            throw GitHubAuthenticationError.requestRejected
        }
        guard data.count <= 65_536 else { throw GitHubAuthenticationError.invalidResponse }
        guard let decoded = try? JSONDecoder().decode(Response.self, from: data) else {
            throw GitHubAuthenticationError.invalidResponse
        }
        return decoded
    }

    private func authenticationError(_ code: String) -> GitHubAuthenticationError {
        switch code {
        case "access_denied": .accessDenied
        case "expired_token", "token_expired": .authorizationExpired
        case "device_flow_disabled": .deviceFlowDisabled
        case "incorrect_device_code": .incorrectDeviceCode
        case "bad_refresh_token": .refreshExpired
        default: .requestRejected
        }
    }

    private static func formEncode(_ value: String) -> String {
        value.utf8.map { byte in
            if isASCIIAlphanumeric(byte) || [45, 46, 95, 126].contains(byte) {
                return String(UnicodeScalar(byte))
            }
            return String(format: "%%%02X", byte)
        }.joined()
    }

    private static func isASCIIAlphanumeric(_ byte: UInt8) -> Bool {
        (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
    }

    private static func isValidDeviceCode(_ code: String) -> Bool {
        code.utf8.count == 40 && code.utf8.allSatisfy { (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }
    }

    private static func isValidUserCode(_ code: String) -> Bool {
        let bytes = Array(code.utf8)
        guard bytes.count == 9, bytes[4] == 45 else { return false }
        return bytes.enumerated().allSatisfy { index, byte in index == 4 || (48...57).contains(byte) || (65...90).contains(byte) }
    }
}

private struct DeviceResponse: Decodable {
    let deviceCode: String?
    let userCode: String?
    let verificationURI: String?
    let expiresIn: Int?
    let interval: Int?
    let error: String?

    enum CodingKeys: String, CodingKey {
        case deviceCode = "device_code"
        case userCode = "user_code"
        case verificationURI = "verification_uri"
        case expiresIn = "expires_in"
        case interval
        case error
    }
}

private struct TokenResponse: Decodable {
    let accessToken: String?
    let expiresIn: Int?
    let refreshToken: String?
    let refreshTokenExpiresIn: Int?
    let tokenType: String?
    let interval: Int?
    let error: String?

    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case expiresIn = "expires_in"
        case refreshToken = "refresh_token"
        case refreshTokenExpiresIn = "refresh_token_expires_in"
        case tokenType = "token_type"
        case interval
        case error
    }
}
