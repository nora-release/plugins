import Foundation
import Testing
@testable import GitHubCore

struct GitHubAvatarsTests {
    @Test func avatarsComeFromGitHubsAvatarHostOnly() {
        #expect(GitHubAvatars.url(login: "vue-hooks-form", avatarURL: nil)?.absoluteString == "https://avatars.githubusercontent.com/vue-hooks-form?s=96")
        #expect(GitHubAvatars.url(login: "x", avatarURL: "https://avatars.githubusercontent.com/u/1?v=4")?.absoluteString
            == "https://avatars.githubusercontent.com/u/1?v=4")
        #expect(GitHubAvatars.url(login: "x", avatarURL: "https://example.com/a.png")?.host == "avatars.githubusercontent.com")
        #expect(GitHubAvatars.url(login: "renovate[bot]", avatarURL: nil) == nil)
        #expect(GitHubAvatars.url(login: "", avatarURL: nil) == nil)
    }

    @Test func aPictureIsAskedOfNoraOnceAndKept() async throws {
        let asked = Counter()
        let avatars = GitHubAvatars { method, parameters in
            await asked.add()
            #expect(method == "network.request")
            #expect(parameters["responseEncoding"].string == "base64")
            return .object(["status": .number(200), "bodyBase64": .string(Data("png".utf8).base64EncodedString())])
        }
        let url = try #require(GitHubAvatars.url(login: "octocat", avatarURL: nil))
        async let first = avatars.data(url)
        async let second = avatars.data(url)
        #expect(await first == Data("png".utf8))
        #expect(await second == Data("png".utf8))
        #expect(await avatars.data(url) == Data("png".utf8))
        #expect(await asked.value == 1)
        #expect(await avatars.data(URL(string: "https://example.com/a.png")!) == nil)
        #expect(await asked.value == 1)
    }

    @Test func aPictureNoraCannotFetchIsNil() async throws {
        let avatars = GitHubAvatars { _, _ in .object(["status": .number(404), "bodyBase64": .string("")]) }
        #expect(await avatars.data(try #require(GitHubAvatars.url(login: "octocat", avatarURL: nil))) == nil)
        let failing = GitHubAvatars { _, _ in throw GitHubAPIError.invalidResponse }
        #expect(await failing.data(try #require(GitHubAvatars.url(login: "octocat", avatarURL: nil))) == nil)
    }
}

private actor Counter {
    private(set) var value = 0
    func add() { value += 1 }
}
