import Foundation
import AppKit
import GitHubCore
import Testing
@testable import GitHubUI

struct GitHubConversationTests {
    private func value(_ source: String) throws -> GitHubValue { try JSONDecoder().decode(GitHubValue.self, from: Data(source.utf8)) }
    private func section(_ values: [GitHubValue]) -> GitHubSectionState {
        var result = GitHubSectionState(); result.items = values; result.loaded = true; return result
    }

    @Test func forcePushesJoinConsecutiveCommitGroupsAndKeepEventIdentity() throws {
        let first = try value(#"{"id":1,"event":"head_ref_force_pushed","commit_id":"aaaaaaaa","created_at":"2026-01-01T00:00:00Z"}"#)
        let second = try value(#"{"id":2,"event":"head_ref_force_pushed","commit_id":"aaaaaaaa","created_at":"2026-01-02T00:00:00Z"}"#)
        let commit = try value(#"{"sha":"bbbbbbbb","event":"committed","message":"Fix a bug","committer":{"date":"2026-01-03T00:00:00Z"}}"#)
        let groups = GitHubConversation.groups(sections: [.timeline: section([first, second, commit])])
        #expect(groups.count == 1)
        #expect(groups[0].isCollection)
        #expect(groups[0].containsForcePush)
        #expect(Set(groups[0].entries.map(\.id)).count == 3)
    }

    @Test func commentsAndReviewsSeparateCommitCollections() throws {
        let commit1 = try value(#"{"sha":"aaaaaaaa","event":"committed","created_at":"2026-01-01T00:00:00Z"}"#)
        let comment = try value(#"{"id":4,"body":"A comment","created_at":"2026-01-02T00:00:00Z"}"#)
        let commit2 = try value(#"{"sha":"bbbbbbbb","event":"committed","created_at":"2026-01-03T00:00:00Z"}"#)
        let groups = GitHubConversation.groups(sections: [.timeline: section([commit2, commit1]), .comments: section([comment])])
        #expect(groups.count == 3)
        #expect(groups[1].entries[0].kind == .comment)
        let hasCollection = groups.contains(where: \.isCollection)
        #expect(!hasCollection)
    }

    @Test func fullCommitListDoesNotDuplicateTimelineCommits() throws {
        let commit = try value(#"{"sha":"aaaaaaaa","event":"committed","message":"Fix","created_at":"2026-01-01T00:00:00Z"}"#)
        let groups = GitHubConversation.groups(sections: [.timeline: section([commit]), .commits: section([commit])])
        #expect(groups.flatMap(\.entries).count == 1)
    }

    @Test func addingAnotherPageKeepsExpandedGroupIdentityStable() throws {
        let first = try value(#"{"id":1,"event":"head_ref_force_pushed","created_at":"2026-01-01T00:00:00Z"}"#)
        let second = try value(#"{"id":2,"event":"head_ref_force_pushed","created_at":"2026-01-02T00:00:00Z"}"#)
        let previous = GitHubConversation.groups(sections: [.timeline: section([first])])
        let updated = GitHubConversation.groups(sections: [.timeline: section([first, second])])
        #expect(previous.first?.id == updated.first?.id)
    }

    @Test func markdownRendersTablesDetailsTasksAndImagesAsNativeBlocks() {
        let blocks = GitHubMarkdownDocument.parse("""
        | Package | Version |
        | --- | --- |
        | nora | 1.0 |

        - [x] Done

        <details><summary>Release notes</summary>

        ## Fix

        Safe text

        </details>

        ![Screenshot](https://example.com/image.png)
        """)
        #expect(blocks.contains { if case .table = $0 { true } else { false } })
        #expect(blocks.contains { if case .taskList = $0 { true } else { false } })
        #expect(blocks.contains { if case .disclosure = $0 { true } else { false } })
        #expect(blocks.contains { if case .image = $0 { true } else { false } })
    }

    @Test func laterCommentsDoNotEraseApprovedReviewerStatus() throws {
        let approved = try value(#"{"id":1,"state":"APPROVED","user":{"login":"reviewer"},"submitted_at":"2026-01-01T00:00:00Z"}"#)
        let comment = try value(#"{"id":2,"state":"COMMENTED","user":{"login":"reviewer"},"submitted_at":"2026-01-02T00:00:00Z"}"#)
        let latest = GitHubReviewers.latest(reviews: [comment, approved], requested: [])
        #expect(latest.count == 1 && latest[0].state == "APPROVED")
        let requested = GitHubReviewers.latest(reviews: [approved], requested: [.object(["login": .string("reviewer")])])
        #expect(requested[0].state == "REQUESTED")
    }

    @Test func markdownKeepsLongDescriptionsAndLinkedImages() {
        let long = String(repeating: "原生内容", count: 8_000) + " end-of-description"
        let blocks = GitHubMarkdownDocument.parse(long)
        guard case let .paragraph(text)? = blocks.last else { Issue.record("Long text did not remain a paragraph"); return }
        #expect(text.hasSuffix("end-of-description"))
        let image = GitHubMarkdownDocument.parse("[![Screenshot](https://example.com/image.png)](https://example.com)")
        #expect(image == [.image(source: "https://example.com/image.png", alt: "Screenshot")])
    }

    @Test func markdownRetainsNativeLinksAndOmitsExecutableHTML() {
        let text = GitHubMarkdownDocument.inline("[Pull request](https://github.com/team/repo/pull/1)")
        #expect(text.runs.first?.link?.absoluteString == "https://github.com/team/repo/pull/1")
        let blocks = GitHubMarkdownDocument.parse("<script>alert('untrusted')</script>\n\nSafe content")
        #expect(blocks == [.paragraph("Safe content")])
    }

    @Test func fullPullRequestDiffCanRecoverAnOmittedFilePatch() {
        let source = """
        diff --git a/first.swift b/first.swift
        --- a/first.swift
        +++ b/first.swift
        @@ -1 +1 @@
        -old
        +new
        diff --git a/second.swift b/second.swift
        --- a/second.swift
        +++ b/second.swift
        @@ -1 +1 @@
        -before
        +after
        """
        let document = GitHubDiffDocument.parse(source)
        #expect(document.patch(fileName: "second.swift") == "@@ -1 +1 @@\n-before\n+after")
        #expect(document.patch(fileName: "unknown.swift") == nil)
    }

    @MainActor @Test func bundledSwiftShikiProducesNativeSyntaxColors() {
        let code = #"{"version": 1, "enabled": true}"#
        let highlighted = GitHubCodeHighlighter().attributedString(code: code, language: "json")
        var hasSyntaxColor = false
        highlighted.enumerateAttribute(.foregroundColor, in: NSRange(location: 0, length: highlighted.length)) { color, _, _ in
            if let color = color as? NSColor, color != .labelColor { hasSyntaxColor = true }
        }
        #expect(highlighted.string == code)
        #expect(hasSyntaxColor)
    }

    @Test func diffMaintainsOldAndNewLineNumbersAcrossChanges() {
        let document = GitHubDiffDocument.parse("@@ -8,3 +8,3 @@\n context\n-old\n+new\n tail\n")
        let lines = document.files.flatMap(\.hunks).flatMap(\.lines)
        #expect(lines.count == 4)
        #expect(lines[1].oldNumber == 9 && lines[1].newNumber == nil)
        #expect(lines[2].newNumber == 9 && lines[2].oldNumber == nil)
        #expect(lines[3].oldNumber == 10 && lines[3].newNumber == 10)
    }
}
