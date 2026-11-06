import AppKit
import SwiftUI
import Testing
@testable import GitHubCore
@testable import GitHubUI

@MainActor
struct GitHubLoadingLayoutTests {
    @Test(arguments: [false, true])
    func skeletonFitsNarrowContainersAndKeepsItsSizeAfterLayout(sidebar: Bool) {
        let host = NSHostingController(rootView: GitHubSkeleton(rows: 4, sidebar: sidebar))
        for width in [CGFloat(120), 210, 340, 640] {
            let proposal = CGSize(width: width, height: 1000)
            let fitted = host.sizeThatFits(in: proposal)
            #expect(fitted.width > 0 && fitted.width <= width + 0.5)
            #expect(fitted.height > 0 && fitted.height < proposal.height)
            host.view.frame.size = fitted
            for _ in 0 ..< 3 {
                host.view.needsLayout = true
                host.view.layoutSubtreeIfNeeded()
                let repeated = host.sizeThatFits(in: proposal)
                #expect(abs(repeated.width - fitted.width) < 0.5)
                #expect(abs(repeated.height - fitted.height) < 0.5)
            }
        }
    }

    @Test func loadingAndDiscussionKeepTheSameViewport() throws {
        for width in [CGFloat(640), 819, 820, 1100] {
            let loading = try discussionViewport(width: width, loading: true)
            let discussion = try discussionViewport(width: width, loading: false)
            #expect(loading.minY > 0)
            #expect(loading.height > 380)
            #expect(abs(loading.width - width) < 0.5)
            #expect(abs(discussion.minX - loading.minX) < 0.5)
            #expect(abs(discussion.minY - loading.minY) < 0.5)
            #expect(abs(discussion.width - loading.width) < 0.5)
            #expect(abs(discussion.height - loading.height) < 0.5)
        }
    }

    private func discussionViewport(width: CGFloat, loading: Bool) throws -> NSRect {
        let service = GitHubService()
        service.isLoadingDetail = loading
        let item: GitHubValue = .object(["id": .number(1), "number": .number(1), "title": .string("A native issue")])
        let host = NSHostingView(rootView: GitHubItemDetailView(service: service, item: item, drafts: .constant([:])))
        host.frame = NSRect(x: 0, y: 0, width: width, height: 760)
        host.layoutSubtreeIfNeeded()
        let scroll = try #require(descendants(of: host).compactMap { $0 as? NSScrollView }.first)
        return host.convert(scroll.bounds, from: scroll)
    }

    private func descendants(of view: NSView) -> [NSView] {
        view.subviews.flatMap { [$0] + descendants(of: $0) }
    }
}
