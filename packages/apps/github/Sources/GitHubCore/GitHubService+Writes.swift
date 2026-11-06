import Foundation

extension GitHubService {
    public func postComment(_ text: String) async {
        guard !isPostingComment, let detail else { return }
        let body = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, body.utf8.count <= 65_536 else {
            commentError = GHC("Enter a comment of at most 65,536 bytes."); return
        }
        guard detail["locked"].bool != true, sections[.repository]?.data["archived"].bool != true else {
            commentError = GHC("This conversation is locked or archived."); return
        }
        let key = Self.itemKey(detail), revision = currentDetailRevision()
        guard !uncertainComments.contains(key) else {
            commentError = GHC("Refresh the conversation before retrying this comment."); return
        }
        isPostingComment = true; commentError = nil; commentNotice = nil
        defer { isPostingComment = false }
        do {
            let api = try await api()
            guard isCurrentDetail(revision, key: key) else { return }
            let response = try await api.request(path: "/repos/\(Self.itemRepository(detail))/issues/\(detail["number"].int ?? 0)/comments", method: "POST", body: .object(["body": .string(body)]), expectedStatuses: [201])
            guard response.data["id"].int != nil, response.data["body"].string != nil else { throw GitHubAPIError.invalidResponse }
            guard isCurrentDetail(revision, key: key) else { return }
            commentNotice = GHC("Comment posted.")
            await loadSection(.comments)
            await loadMoreDiscussion()
        } catch is CancellationError {} catch {
            guard isCurrentDetail(revision, key: key) else { return }
            if (error as? GitHubAPIError) == .networkUnavailable || (error as? GitHubAPIError) == .invalidResponse { uncertainComments.insert(key) }
            commentError = error is GitHubAPIError && (error as? GitHubAPIError) == .networkUnavailable
                ? GHC("The comment result could not be confirmed. Refresh the conversation before trying again.") : error.localizedDescription
        }
    }
    public func mergeProblem(method: GitHubMergeMethod, item: GitHubValue? = nil, repository: GitHubValue? = nil) -> String? {
        guard let item = item ?? detail else { return GHC("Select a pull request.") }
        let repo = repository ?? sections[.repository]?.data ?? .null
        if kind != .pullRequests || item["state"].text != "open" || item["merged"].bool == true || !item["merged_at"].isNull { return GHC("This PR is already closed or merged.") }
        if item["draft"].bool == true { return GHC("A draft PR cannot be merged.") }
        if repo["archived"].bool == true { return GHC("This repository is archived.") }
        if repo["permissions"]["push"].bool != true { return GHC("Write access to this repository is required to merge.") }
        if repo[method.repositorySetting].bool != true { return GHC("This merge method is not enabled for this repository.") }
        if !Self.isSHA(item["head"]["sha"].text) || item["base"]["ref"].text.isEmpty { return GHC("The latest commit or target branch is unavailable. Refresh this PR.") }
        if item["mergeable"].bool == false || item["mergeable_state"].text == "dirty" { return GHC("This PR has conflicts. Resolve them before merging.") }
        if item["mergeable_state"].text == "blocked" { return GHC("Repository rules currently block merging.") }
        if item["mergeable_state"].text == "behind" { return GHC("Update the source branch before merging.") }
        if item["mergeable"].bool != true || item["mergeable_state"].text == "unknown" { return GHC("GitHub is calculating merge status. Refresh to check again.") }
        return nil
    }
    public func prepareMerge(_ method: GitHubMergeMethod) async {
        guard !isMerging, let detail else { return }
        mergeError = nil; mergeProposal = nil
        if let problem = mergeProblem(method: method) { mergeError = problem; return }
        mergeProposal = GitHubMergeProposal(method: method, repository: Self.itemRepository(detail), number: detail["number"].int ?? 0,
            headSHA: detail["head"]["sha"].text.lowercased(), baseRef: detail["base"]["ref"].text,
            baseRepository: detail["base"]["repo"]["full_name"].string ?? Self.itemRepository(detail))
    }
    public func cancelMerge() { guard !isMerging else { return }; mergeProposal = nil; mergeError = nil }
    public func confirmMerge() async {
        guard !isMerging, let proposal = mergeProposal, let detail else { return }
        let key = Self.itemKey(detail), revision = currentDetailRevision()
        guard Self.itemRepository(detail) == proposal.repository, detail["number"].int == proposal.number,
              detail["head"]["sha"].text.lowercased() == proposal.headSHA else { mergeProposal = nil; return }
        isMerging = true; mergeError = nil
        defer { isMerging = false }
        do {
            let api = try await api()
            let fresh = try await api.request(path: "/repos/\(proposal.repository)/pulls/\(proposal.number)").data
            guard isCurrentDetail(revision, key: key), mergeProposal == proposal else { return }
            guard fresh["number"].int == proposal.number, fresh["head"]["sha"].text.lowercased() == proposal.headSHA,
                  fresh["base"]["ref"].text == proposal.baseRef,
                  (fresh["base"]["repo"]["full_name"].string ?? proposal.repository).caseInsensitiveCompare(proposal.baseRepository) == .orderedSame else { throw GitHubAPIError.conflict }
            let repoResponse = try await api.request(path: "/repos/\(proposal.repository)")
            guard repoResponse.data["full_name"].text.caseInsensitiveCompare(proposal.repository) == .orderedSame else { throw GitHubAPIError.invalidResponse }
            let repository = try await api.completeMergeSettings(repository: proposal.repository, data: repoResponse.data)
            guard isCurrentDetail(revision, key: key), mergeProposal == proposal else { return }
            if let problem = mergeProblem(method: proposal.method, item: fresh, repository: repository) {
                mergeError = problem; mergeProposal = nil; return
            }
            let response = try await api.request(path: "/repos/\(proposal.repository)/pulls/\(proposal.number)/merge", method: "PUT",
                body: .object(["sha": .string(proposal.headSHA), "merge_method": .string(proposal.method.rawValue)]))
            guard response.data["merged"].bool == true else { throw GitHubAPIError.validationFailed }
            guard isCurrentDetail(revision, key: key) else { return }
            self.detail = detail.merging(fresh).merging(.object(["state": .string("closed"), "merged": .bool(true), "merge_commit_sha": response.data["sha"]]))
            selectedItem = self.detail; mergeProposal = nil
            removeFeedItem(key: key)
            await present()
            await loadSection(.timeline)
        } catch is CancellationError {} catch {
            guard isCurrentDetail(revision, key: key) else { return }
            mergeProposal = nil
            mergeError = (error as? GitHubAPIError) == .networkUnavailable
                ? GHC("The merge result could not be confirmed. Refresh the PR before trying again.") : error.localizedDescription
        }
    }
}
