import Foundation
import Testing
@testable import GitHubCore

@Suite("GitHub authentication")
struct GitHubAuthenticationTests {
    private static let deviceCode = "1234567890abcdef1234567890abcdef12345678"
    private static let start = Date(timeIntervalSince1970: 1_700_000_000)

    @Test("Device authorization uses the configured public Client ID")
    func deviceAuthorization() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([Self.deviceResponse])
        let login = Self.login(transport, clock)
        let authorization = try await login.begin()
        #expect(authorization.deviceCode == Self.deviceCode)
        #expect(authorization.userCode == "ABCD-EFGH")
        #expect(authorization.verificationURL.absoluteString == "https://github.com/login/device")
        #expect(authorization.expiresAt == Self.start.addingTimeInterval(900))
        #expect(authorization.interval == 5)
        let requests = await transport.requests
        let request = try #require(requests.first)
        #expect(request.url?.absoluteString == "https://github.com/login/device/code")
        #expect(request.httpMethod == "POST")
        #expect(request.value(forHTTPHeaderField: "Accept") == "application/json")
        #expect(request.value(forHTTPHeaderField: "Content-Type") == "application/x-www-form-urlencoded")
        #expect(String(data: request.httpBody ?? Data(), encoding: .utf8) == "client_id=Iv1.test-app")
        #expect(request.value(forHTTPHeaderField: "Authorization") == nil)
    }

    @Test("Pending authorization polls with cumulative slow-down intervals")
    func pollingIntervals() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([
            Self.deviceResponse,
            #"{"error":"authorization_pending"}"#,
            #"{"error":"slow_down","interval":10}"#,
            #"{"error":"slow_down","interval":20}"#,
            Self.expiringTokenResponse,
        ])
        let login = Self.login(transport, clock)
        let authorization = try await login.begin()
        let credentials = try await login.wait(for: authorization)
        #expect(clock.waits == [5, 5, 10, 20])
        #expect(credentials.token == "ghu_access_test")
        #expect(credentials.expiresAt == Self.start.addingTimeInterval(40 + 28_800))
        #expect(credentials.refreshToken == "ghr_refresh_test")
        #expect(credentials.refreshExpiresAt == Self.start.addingTimeInterval(40 + 15_897_600))
        let requests = await transport.requests
        let request = try #require(requests.last)
        let body = try #require(String(data: request.httpBody ?? Data(), encoding: .utf8))
        #expect(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"))
        #expect(body.contains("device_code=\(Self.deviceCode)"))
        #expect(!body.contains("client_secret"))
    }

    @Test("Local authorization expiration prevents a request after the deadline")
    func localExpiration() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([#"{"error":"authorization_pending"}"#])
        let authorization = Self.authorization(expiresAt: Self.start.addingTimeInterval(7))
        await #expect(throws: GitHubAuthenticationError.authorizationExpired) {
            try await Self.login(transport, clock).wait(for: authorization)
        }
        #expect(clock.waits == [5, 2])
        #expect(await transport.requests.count == 1)
    }

    @Test("Already expired authorization makes no HTTP requests")
    func alreadyExpired() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([])
        await #expect(throws: GitHubAuthenticationError.authorizationExpired) {
            try await Self.login(transport, clock).wait(for: Self.authorization(expiresAt: Self.start))
        }
        #expect(clock.waits.isEmpty)
        #expect(await transport.requests.isEmpty)
    }

    @Test("Canceling login interrupts polling without completing authorization")
    func cancellation() async throws {
        let signal = AuthenticationTestSignal()
        let transport = AuthenticationTestTransport([])
        let login = GitHubDeviceLogin(
            clientID: "Iv1.test-app",
            transport: transport,
            now: { Self.start },
            sleep: { _ in
                await signal.signal()
                try await Task.sleep(for: .seconds(10_000))
            }
        )
        let task = Task { try await login.wait(for: Self.authorization()) }
        await signal.wait()
        task.cancel()
        await #expect(throws: CancellationError.self) { try await task.value }
        #expect(await transport.requests.isEmpty)
    }

    @Test("GitHub device errors have stable safe messages", arguments: [
        ("access_denied", GitHubAuthenticationError.accessDenied),
        ("expired_token", .authorizationExpired),
        ("token_expired", .authorizationExpired),
        ("device_flow_disabled", .deviceFlowDisabled),
        ("incorrect_device_code", .incorrectDeviceCode),
        ("incorrect_client_credentials", .requestRejected),
        ("unexpected_private_server_text", .requestRejected),
    ])
    func deviceErrors(code: String, expected: GitHubAuthenticationError) async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport(["{\"error\":\"\(code)\",\"error_description\":\"private_token_should_not_escape\"}"])
        await #expect(throws: expected) {
            try await Self.login(transport, clock).wait(for: Self.authorization())
        }
        #expect(!expected.localizedDescription.contains("private_token_should_not_escape"))
    }

    @Test("Invalid Client IDs fail before making a request", arguments: ["", " ", "a&scope=repo", "abc\nxyz", "🔒", String(repeating: "a", count: 257)])
    func invalidClientID(_ clientID: String) async throws {
        let transport = AuthenticationTestTransport([])
        await #expect(throws: GitHubAuthenticationError.invalidClientID) {
            try await GitHubDeviceLogin(clientID: clientID, transport: transport).begin()
        }
        #expect(await transport.requests.isEmpty)
    }

    @Test("Malformed authorization data is rejected", arguments: [
        #"{}"#,
        #"{"device_code":"1234567890abcdef1234567890abcdef12345678","user_code":"ABCD-EFGH","verification_uri":"https://github.com.attacker.test/login/device","expires_in":900,"interval":5}"#,
        #"{"device_code":"1234567890abcdef1234567890abcdef12345678","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device?other=1","expires_in":900,"interval":5}"#,
        #"{"device_code":"1234567890abcdef1234567890abcdef12345678","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":0,"interval":5}"#,
        #"{"device_code":"1234567890abcdef1234567890abcdef12345678","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":900,"interval":0}"#,
        #"{"device_code":"secret\ninvalid","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
        #"{"device_code":"1234567890abcdef1234567890abcdef12345678","user_code":"bad code","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
        #"{"device_code":"1234567890abcdef1234567890abcdef12345678","user_code":"ABCD-EFGH","verification_uri":"https://github.com/login/device","expires_in":900,"interval":1.5}"#,
    ])
    func malformedAuthorization(_ body: String) async throws {
        let transport = AuthenticationTestTransport([body])
        await #expect(throws: GitHubAuthenticationError.invalidResponse) {
            try await GitHubDeviceLogin(clientID: "Iv1.test-app", transport: transport).begin()
        }
    }

    @Test("Malformed access token responses never produce credentials", arguments: [
        #"{}"#,
        #"{"access_token":"private_value","token_type":"basic"}"#,
        #"{"access_token":"private\r\nvalue","token_type":"bearer"}"#,
        #"{"access_token":"private_value","token_type":"bearer","expires_in":-1}"#,
        #"{"access_token":"private_value","token_type":"bearer","refresh_token":"private_refresh"}"#,
        #"{"access_token":"private_value","token_type":"bearer","refresh_token_expires_in":300}"#,
        #"{"error":"slow_down","interval":0}"#,
        "<html>private_value</html>",
    ])
    func malformedTokens(_ body: String) async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([body])
        await #expect(throws: GitHubAuthenticationError.invalidResponse) {
            try await Self.login(transport, clock).wait(for: Self.authorization())
        }
    }

    @Test("Nonexpiring access tokens remain usable without refresh data")
    func nonexpiringToken() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([#"{"access_token":"ghu_access_test","token_type":"bearer"}"#])
        let credentials = try await Self.login(transport, clock).wait(for: Self.authorization())
        #expect(credentials.expiresAt == nil)
        #expect(credentials.refreshToken == nil)
        #expect(!credentials.isExpired(at: Self.start.addingTimeInterval(10_000_000)))
    }

    @Test("Device flow refresh rotates both tokens without a Client Secret")
    func refreshRotatesTokens() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([Self.expiringTokenResponse])
        let oldCredentials = GitHubCredentials(
            token: "ghu_old_access",
            expiresAt: Self.start.addingTimeInterval(-1),
            refreshToken: "ghr_old+refresh",
            refreshExpiresAt: Self.start.addingTimeInterval(3600)
        )
        let refreshed = try await Self.login(transport, clock).refresh(oldCredentials)
        #expect(refreshed.token == "ghu_access_test")
        #expect(refreshed.refreshToken == "ghr_refresh_test")
        #expect(refreshed.expiresAt == Self.start.addingTimeInterval(28_800))
        #expect(refreshed.refreshExpiresAt == Self.start.addingTimeInterval(15_897_600))
        let requests = await transport.requests
        let request = try #require(requests.first)
        let body = try #require(String(data: request.httpBody ?? Data(), encoding: .utf8))
        #expect(body == "client_id=Iv1.test-app&grant_type=refresh_token&refresh_token=ghr_old%2Brefresh")
        #expect(!body.contains("client_secret"))
    }

    @Test("Missing or expired refresh credentials make no requests")
    func unusableRefreshCredentials() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([])
        let login = Self.login(transport, clock)
        await #expect(throws: GitHubAuthenticationError.refreshUnavailable) {
            try await login.refresh(GitHubCredentials(token: "github_pat_test"))
        }
        await #expect(throws: GitHubAuthenticationError.refreshExpired) {
            try await login.refresh(GitHubCredentials(token: "ghu_access_test", refreshToken: "ghr_refresh_test", refreshExpiresAt: Self.start))
        }
        #expect(await transport.requests.isEmpty)
    }

    @Test("Revoked refresh credentials require reconnection")
    func revokedRefreshToken() async throws {
        let clock = AuthenticationTestClock(Self.start)
        let transport = AuthenticationTestTransport([#"{"error":"bad_refresh_token","error_description":"private_token"}"#])
        await #expect(throws: GitHubAuthenticationError.refreshExpired) {
            try await Self.login(transport, clock).refresh(GitHubCredentials(token: "ghu_access", refreshToken: "ghr_refresh"))
        }
    }

    @Test("Service and transport failures discard response and error text")
    func safeFailures() async throws {
        let clock = AuthenticationTestClock(Self.start)
        for (status, expected) in [(429, GitHubAuthenticationError.rateLimited), (503, .serviceUnavailable), (403, .requestRejected)] {
            let transport = AuthenticationTestTransport([#"{"private":"token_should_not_escape"}"#], status: status)
            await #expect(throws: expected) {
                try await Self.login(transport, clock).begin()
            }
            #expect(!expected.localizedDescription.contains("token_should_not_escape"))
        }
        let transport = AuthenticationTestTransport([], fails: true)
        await #expect(throws: GitHubAuthenticationError.connectionFailed) {
            try await Self.login(transport, clock).begin()
        }
    }

    @Test("Credentials preserve optional expiry and redact private data")
    func credentialsStorage() throws {
        let credentials = GitHubCredentials(token: "private_access", expiresAt: Self.start, refreshToken: "private_refresh", refreshExpiresAt: Self.start.addingTimeInterval(30))
        #expect(credentials.isExpired(at: Self.start))
        #expect(!credentials.isExpired(at: Self.start.addingTimeInterval(-1)))
        let data = try JSONEncoder().encode(credentials)
        #expect(try JSONDecoder().decode(GitHubCredentials.self, from: data) == credentials)
        let tokenOnlyData = Data(#"{"token":"github_pat_test"}"#.utf8)
        let tokenOnlyCredentials = try JSONDecoder().decode(GitHubCredentials.self, from: tokenOnlyData)
        #expect(tokenOnlyCredentials.expiresAt == nil)
        #expect(tokenOnlyCredentials.refreshToken == nil)
        #expect(!String(describing: credentials).contains("private_access"))
        #expect(!String(reflecting: credentials).contains("private_refresh"))
        #expect(!String(reflecting: Self.authorization()).contains(Self.deviceCode))
    }

    @Test("Invalid credentials are rejected before the native host store is called")
    func invalidCredentialWrite() async {
        let host = CoreHostFixture()
        let store = GitHubHostCredentialStore { method, value in try await host.request(method, value) }
        await #expect(throws: GitHubAPIError.invalidCredential) {
            try await store.save(GitHubCredentials(token: "invalid\nheader"))
        }
        await #expect(throws: GitHubAPIError.invalidCredential) {
            try await store.save(GitHubCredentials(token: "", refreshToken: "private_refresh"))
        }
        #expect(await host.methods.isEmpty)
    }

    private static var deviceResponse: String {
        "{\"device_code\":\"\(deviceCode)\",\"user_code\":\"ABCD-EFGH\",\"verification_uri\":\"https://github.com/login/device\",\"expires_in\":900,\"interval\":5}"
    }

    private static var expiringTokenResponse: String {
        #"{"access_token":"ghu_access_test","expires_in":28800,"refresh_token":"ghr_refresh_test","refresh_token_expires_in":15897600,"token_type":"bearer"}"#
    }

    private static func authorization(expiresAt: Date = start.addingTimeInterval(900)) -> GitHubDeviceAuthorization {
        GitHubDeviceAuthorization(deviceCode: deviceCode, userCode: "ABCD-EFGH", verificationURL: URL(string: "https://github.com/login/device")!, expiresAt: expiresAt, interval: 5)
    }

    private static func login(_ transport: AuthenticationTestTransport, _ clock: AuthenticationTestClock) -> GitHubDeviceLogin {
        GitHubDeviceLogin(clientID: "Iv1.test-app", transport: transport, now: { clock.now }, sleep: { clock.advance($0) })
    }
}

private final class AuthenticationTestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var date: Date
    private var recordedWaits: [TimeInterval] = []

    init(_ date: Date) { self.date = date }
    var now: Date { lock.withLock { date } }
    var waits: [TimeInterval] { lock.withLock { recordedWaits } }

    func advance(_ interval: TimeInterval) {
        lock.withLock {
            recordedWaits.append(interval)
            date.addTimeInterval(interval)
        }
    }
}

private actor AuthenticationTestTransport: GitHubHTTPTransport {
    private var responses: [String]
    private let status: Int
    private let fails: Bool
    private(set) var requests: [URLRequest] = []

    init(_ responses: [String], status: Int = 200, fails: Bool = false) {
        self.responses = responses
        self.status = status
        self.fails = fails
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        requests.append(request)
        if fails { throw AuthenticationTestTransportFailure() }
        guard !responses.isEmpty, let url = request.url,
              let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: nil) else {
            throw AuthenticationTestTransportFailure()
        }
        return (Data(responses.removeFirst().utf8), response)
    }
}

private struct AuthenticationTestTransportFailure: Error, LocalizedError {
    var errorDescription: String? { "private_token_should_not_escape" }
}

private actor AuthenticationTestSignal {
    private var isSignaled = false
    private var continuation: CheckedContinuation<Void, Never>?

    func signal() {
        isSignaled = true
        continuation?.resume()
        continuation = nil
    }

    func wait() async {
        if isSignaled { return }
        await withCheckedContinuation { continuation = $0 }
    }
}
