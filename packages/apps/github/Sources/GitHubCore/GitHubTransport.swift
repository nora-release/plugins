import Foundation

public protocol GitHubHTTPTransport: Sendable {
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
}

public typealias GitHubHostRequest = @Sendable (String, GitHubValue) async throws -> GitHubValue

public struct GitHubHostTransport: GitHubHTTPTransport {
    private let request: GitHubHostRequest
    public init(request: @escaping GitHubHostRequest) { self.request = request }
    public func send(_ value: URLRequest) async throws -> (Data, HTTPURLResponse) {
        guard let url = value.url else { throw GitHubAPIError.invalidResponse }
        var input: [String: GitHubValue] = [
            "url": .string(url.absoluteString), "method": .string(value.httpMethod ?? "GET"),
            "headers": .object((value.allHTTPHeaderFields ?? [:]).mapValues(GitHubValue.string)),
        ]
        if let body = value.httpBody { input["body"] = .string(String(decoding: body, as: UTF8.self)) }
        let result = try await request("network.request", .object(input))
        guard let status = result["status"].int, let text = result["bodyText"].string,
              let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: nil,
                  headerFields: result["headers"].object.compactMapValues(\.string)) else { throw GitHubAPIError.invalidResponse }
        return (Data(text.utf8), response)
    }
}
