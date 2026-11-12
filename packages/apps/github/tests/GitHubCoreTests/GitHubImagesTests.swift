import Foundation
import Testing
@testable import GitHubCore

struct GitHubImagesTests {
    @Test func avatarsComeFromGitHubsAvatarHostOnly() {
        #expect(GitHubImages.url(login: "vue-hooks-form", avatarURL: nil)?.absoluteString == "https://avatars.githubusercontent.com/vue-hooks-form?s=96")
        #expect(GitHubImages.url(login: "x", avatarURL: "https://avatars.githubusercontent.com/u/1?v=4")?.absoluteString
            == "https://avatars.githubusercontent.com/u/1?v=4")
        #expect(GitHubImages.url(login: "x", avatarURL: "https://example.com/a.png")?.host == "avatars.githubusercontent.com")
        #expect(GitHubImages.url(login: "renovate[bot]", avatarURL: nil) == nil)
        #expect(GitHubImages.url(login: "", avatarURL: nil) == nil)
    }

    @Test func aPictureIsAskedOfNoraOnceAndKept() async throws {
        let asked = Counter()
        let images = GitHubImages { method, parameters in
            await asked.add()
            #expect(method == "network.request")
            #expect(parameters["responseEncoding"].string == "base64")
            return .object(["status": .number(200), "bodyBase64": .string(Data("png".utf8).base64EncodedString())])
        }
        let url = try #require(GitHubImages.url(login: "octocat", avatarURL: nil))
        async let first = images.data(url)
        async let second = images.data(url)
        #expect(await first == Data("png".utf8))
        #expect(await second == Data("png".utf8))
        #expect(await images.data(url) == Data("png".utf8))
        #expect(await asked.value == 1)
        #expect(await images.data(URL(string: "https://example.com/a.png")!) == nil)
        #expect(await asked.value == 1)
    }

    @Test func aPictureNoraCannotFetchIsNil() async throws {
        let images = GitHubImages { _, _ in .object(["status": .number(404), "bodyBase64": .string("")]) }
        #expect(await images.data(try #require(GitHubImages.url(login: "octocat", avatarURL: nil))) == nil)
        let failing = GitHubImages { _, _ in throw GitHubAPIError.invalidResponse }
        #expect(await failing.data(try #require(GitHubImages.url(login: "octocat", avatarURL: nil))) == nil)
    }
}

private actor Counter {
    private(set) var value = 0
    func add() { value += 1 }
}
