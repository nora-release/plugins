import Foundation
import Testing
@testable import GitHubCore

struct GitHubImageSourcesTests {
    private let html = """
    <p><a href="https://github.com/user-attachments/assets/7507778a-25f6-4b4a-be59-181c8b07a796"><img alt="image" \
    src="https://private-user-images.githubusercontent.com/3976973/701725840-7507778a-25f6-4b4a-be59-181c8b07a796.png?jwt=abc&amp;x=1"></a></p>
    <p><img src="https://camo.githubusercontent.com/abc/68747470733a2f2f696d672e736869656c64732e696f2f612e737667" \
    data-canonical-src="https://img.shields.io/a.svg"></p>
    <p><img src="https://camo.githubusercontent.com/def/68747470733a2f2f6578616d706c652e636f6d2f622e706e67"></p>
    <p><img src="https://example.com/not-served-by-github.png"></p>
    """

    @Test func anAttachmentLoadsFromItsSignedLink() throws {
        let rendered = GitHubImageSources.rendered(html)
        let url = GitHubImageSources.resolve("https://github.com/user-attachments/assets/7507778a-25f6-4b4a-be59-181c8b07a796", in: rendered)
        #expect(url?.host == "private-user-images.githubusercontent.com")
        #expect(url?.query == "jwt=abc&x=1")
    }

    @Test func aPictureFromElsewhereLoadsThroughGitHubsProxy() {
        let rendered = GitHubImageSources.rendered(html)
        #expect(GitHubImageSources.resolve("https://img.shields.io/a.svg", in: rendered)?.absoluteString
            == "https://camo.githubusercontent.com/abc/68747470733a2f2f696d672e736869656c64732e696f2f612e737667")
        // Without data-canonical-src, the proxy's last path segment names the address.
        #expect(GitHubImageSources.resolve("https://example.com/b.png", in: rendered)?.host == "camo.githubusercontent.com")
    }

    @Test func onlyPicturesGitHubServesAreFetched() {
        let rendered = GitHubImageSources.rendered(html)
        #expect(rendered.count == 3)
        #expect(GitHubImageSources.resolve("https://example.com/not-served-by-github.png", in: rendered) == nil)
        #expect(GitHubImageSources.resolve("https://user-images.githubusercontent.com/1/a.png", in: [])?.host == "user-images.githubusercontent.com")
        #expect(GitHubImageSources.resolve("https://github.com/user-attachments/assets/00000000-0000-0000-0000-000000000000", in: rendered) == nil)
    }

    @MainActor @Test func aDetailAndItsDiscussionAskForRenderedBodies() async {
        let host = CoreHostFixture()
        let service = GitHubService { method, parameters in try await host.request(method, parameters) }
        await service.restore()
        await service.selectItem(.object(["id": .number(1), "number": .number(1), "title": .string("Change"), "state": .string("open"),
                                          "pull_request": .object([:]), "repository": .object(["full_name": .string("org/repo")])]))
        let accepts = await host.networkRequests.reduce(into: [String: String]()) { result, request in
            let path = URL(string: request["url"].text)?.path ?? ""
            result[path] = request["headers"]["Accept"].text
        }
        #expect(accepts["/repos/org/repo/pulls/1"] == "application/vnd.github.full+json")
        #expect(accepts["/repos/org/repo/issues/1/comments"] == "application/vnd.github.full+json")
        #expect(accepts["/repos/org/repo/pulls/1/files"] != "application/vnd.github.full+json")
    }
}
