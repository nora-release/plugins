import Foundation

/// Avatars, fetched through Nora like every other request: the extension's sandbox has no network of its
/// own. Each picture is asked for once, and kept.
public actor GitHubAvatars {
    public static let host = "avatars.githubusercontent.com"
    private static let maximumBytes = 512 * 1024
    private static let kept = 400

    private let request: GitHubHostRequest
    private var pictures: [URL: Data] = [:]
    private var order: [URL] = []
    private var loading: [URL: Task<Data?, Never>] = [:]

    public init(request: @escaping GitHubHostRequest) { self.request = request }

    /// The avatar of a person or organization by login, or at the URL GitHub gave for it; nil for any
    /// other address.
    public nonisolated static func url(login: String, avatarURL: String?) -> URL? {
        if let avatarURL, let url = URL(string: avatarURL), url.scheme == "https", url.host == host { return url }
        guard !login.isEmpty, login.count <= 100,
              login.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") }) else { return nil }
        return URL(string: "https://\(host)/\(login)?s=96")
    }

    /// The picture's bytes, or nil when it can't be had.
    public func data(_ url: URL) async -> Data? {
        guard url.scheme == "https", url.host == Self.host else { return nil }
        if let picture = pictures[url] { return picture }
        if let task = loading[url] { return await task.value }
        let request = request
        let task = Task<Data?, Never> {
            let input: GitHubValue = .object([
                "url": .string(url.absoluteString), "method": .string("GET"), "responseEncoding": .string("base64"),
                "maximumResponseBytes": .number(Double(Self.maximumBytes)), "timeoutSeconds": .number(30),
            ])
            guard let result = try? await request("network.request", input), result["status"].int == 200,
                  let encoded = result["bodyBase64"].string else { return nil }
            return Data(base64Encoded: encoded)
        }
        loading[url] = task
        let picture = await task.value
        loading[url] = nil
        if let picture { keep(picture, for: url) }
        return picture
    }

    private func keep(_ picture: Data, for url: URL) {
        pictures[url] = picture
        order.append(url)
        while order.count > Self.kept { pictures[order.removeFirst()] = nil }
    }
}
