import Foundation

extension GitHubService {
    public func selectItem(_ item: GitHubValue) async {
        closeDetails()
        selectedItem = item; detail = item; isLoadingDetail = true
        let revision = currentDetailRevision(), key = Self.itemKey(item)
        await present()
        do {
            let api = try await api(), repo = Self.itemRepository(item)
            guard GitHubAPI.isValidRepository(repo), let number = item["number"].int, number > 0 else { throw GitHubAPIError.invalidResponse }
            let result = try await api.request(path: "/repos/\(repo)/\(kind == .pullRequests ? "pulls" : "issues")/\(number)").data
            guard isCurrentDetail(revision, key: key) else { return }
            guard result["number"].int == number, ["open", "closed"].contains(result["state"].text) else { throw GitHubAPIError.invalidResponse }
            let value = item.merging(result).setting("repository", item["repository"].setting("full_name", .string(repo)))
            detail = value; selectedItem = value
            if let index = items.firstIndex(where: { Self.itemKey($0) == key }) { items[index] = value }
            isLoadingDetail = false
            await present()
            let names: [GitHubSection] = kind == .pullRequests ? [.repository, .comments, .reviews, .reviewComments, .timeline, .checks, .statuses, .workflows, .commits] : [.repository, .comments, .timeline]
            await withTaskGroup(of: Void.self) { group in
                for name in names { group.addTask { await self.loadSection(name) } }
            }
            if kind == .pullRequests { await loadAll(.commits) }
            await loadMoreDiscussion()
            if isCurrentDetail(revision, key: key), sections[.comments]?.error == nil { uncertainComments.remove(key) }
        } catch is CancellationError {} catch {
            if isCurrentDetail(revision, key: key) { detailError = error.localizedDescription; isLoadingDetail = false }
        }
    }
    public func loadSection(_ section: GitHubSection, append: Bool = false) async {
        guard let detail else { return }
        let key = Self.itemKey(detail), revision = currentDetailRevision()
        var target = sections[section] ?? GitHubSectionState()
        guard !target.loading, !append || target.nextPage != nil else { return }
        let page = append ? target.nextPage! : 1
        target.loading = true; target.error = nil
        sections[section] = target
        defer {
            if isCurrentDetail(revision, key: key) { sections[section]?.loading = false }
        }
        do {
            let api = try await api(), repo = Self.itemRepository(detail), number = detail["number"].int ?? 0
            let base = "/repos/\(repo)", pull = "\(base)/pulls/\(number)", issue = "\(base)/issues/\(number)"
            let sha = detail["head"]["sha"].text
            var path: String
            switch section {
            case .repository: path = base
            case .comments: path = issue + "/comments"
            case .reviews: path = pull + "/reviews"
            case .reviewComments: path = pull + "/comments"
            case .timeline: path = issue + "/timeline"
            case .checks: path = base + "/commits/\(sha)/check-runs"
            case .statuses: path = base + "/commits/\(sha)/status"
            case .workflows: path = base + "/actions/runs?head_sha=\(sha)"
            case .commits: path = pull + "/commits"
            case .files: path = pull + "/files"
            }
            if [.checks, .statuses, .workflows].contains(section), !Self.isSHA(sha) { throw GitHubAPIError.invalidResponse }
            if section != .repository { path += (path.contains("?") ? "&" : "?") + "per_page=100&page=\(page)" }
            var response = try await api.request(path: path)
            guard isCurrentDetail(revision, key: key) else { return }
            if section == .repository {
                guard response.data["full_name"].text.caseInsensitiveCompare(repo) == .orderedSame else { throw GitHubAPIError.invalidResponse }
                let settings = try await api.completeMergeSettings(repository: repo, data: response.data)
                response = GitHubResult(data: settings, headers: response.headers, status: response.status)
            }
            let rows: GitHubValue
            switch section {
            case .checks: rows = response.data["check_runs"]
            case .statuses: rows = response.data["statuses"]
            case .workflows: rows = response.data["workflow_runs"]
            case .repository: rows = .array([])
            default: rows = response.data
            }
            guard case .array(let incoming) = rows else { throw GitHubAPIError.invalidResponse }
            var seen = Set<String>()
            let all = (append ? target.items : []) + incoming.filter { value in
                ![.checks, .workflows].contains(section) || value["head_sha"].isNull || value["head_sha"].text.lowercased() == sha.lowercased()
            }
            let deduplicated = all.filter { value in
                let rowKey = section == .files ? value["filename"].text : value.id.isEmpty ? value["commit_id"].text : value.id
                return !rowKey.isEmpty && seen.insert(rowKey).inserted
            }
            guard isCurrentDetail(revision, key: key) else { return }
            target.items = deduplicated; target.data = response.data; target.loaded = true
            target.nextPage = response.hasNext && page < 10_000 ? page + 1 : nil; target.loading = false
            sections[section] = target
            if section == .repository { await present() }
        } catch is CancellationError {} catch {
            if isCurrentDetail(revision, key: key) { sections[section]?.error = error.localizedDescription }
        }
    }
    public func loadMoreDiscussion() async {
        let key = detail.map(Self.itemKey), revision = currentDetailRevision()
        for name in [GitHubSection.comments, .reviews, .reviewComments, .timeline] {
            while sections[name]?.nextPage != nil, sections[name]?.error == nil {
                guard sections[name]?.loading != true else { break }
                guard detail.map(Self.itemKey) == key, currentDetailRevision() == revision else { return }
                await loadSection(name, append: true)
            }
        }
    }
    public func loadAll(_ section: GitHubSection) async {
        if sections[section]?.loaded != true { await loadSection(section) }
        let key = detail.map(Self.itemKey), revision = currentDetailRevision()
        while sections[section]?.nextPage != nil, sections[section]?.error == nil {
            guard sections[section]?.loading != true else { break }
            guard detail.map(Self.itemKey) == key, currentDetailRevision() == revision else { return }
            await loadSection(section, append: true)
        }
    }
    public func pullRequestDiff() async throws -> String {
        guard let detail, kind == .pullRequests else { throw GitHubAPIError.invalidResponse }
        let key = Self.itemKey(detail) + ":" + detail["head"]["sha"].text
        if let cachedDiff, cachedDiff.0 == key { return cachedDiff.1 }
        let api = try await api()
        let result = try await api.request(path: "/repos/\(Self.itemRepository(detail))/pulls/\(detail["number"].int ?? 0)", accept: "application/vnd.github.v3.diff", decodeJSON: false)
        guard let patch = result.data.string else { throw GitHubAPIError.invalidResponse }
        if self.detail.map({ Self.itemKey($0) + ":" + $0["head"]["sha"].text }) == key { cachedDiff = (key, patch) }
        return patch
    }
    public func workflowJobs(id: Int) async throws -> [GitHubValue] {
        guard id > 0, let detail else { throw GitHubAPIError.invalidResponse }
        let api = try await api(), repo = Self.itemRepository(detail)
        var next: Int? = 1, values: [GitHubValue] = []
        while let page = next {
            let batch = try await api.list(path: "/repos/\(repo)/actions/runs/\(id)/jobs", page: page, envelope: "jobs")
            values += batch.items; next = batch.nextPage
        }
        return values
    }
    public func rerunWorkflow(id: Int) async { await workflowAction(id: id, action: "rerun") }
    public func cancelWorkflow(id: Int) async { await workflowAction(id: id, action: "cancel") }
    private func workflowAction(id: Int, action: String) async {
        guard id > 0, let detail else { return }
        do {
            let api = try await api()
            _ = try await api.request(path: "/repos/\(Self.itemRepository(detail))/actions/runs/\(id)/\(action)", method: "POST", expectedStatuses: [201, 202])
            await loadSection(.workflows)
        } catch { self.error = error.localizedDescription }
    }
    nonisolated static func isSHA(_ value: String) -> Bool { value.utf8.count == 40 && value.utf8.allSatisfy { (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) } }
}

extension GitHubAPI {
    func completeMergeSettings(repository: String, data: GitHubValue) async throws -> GitHubValue {
        guard data["permissions"]["push"].bool == true, data["archived"].bool != true,
              GitHubMergeMethod.allCases.contains(where: { data[$0.repositorySetting].bool == nil }) else { return data }
        let parts = repository.split(separator: "/").map(String.init)
        let response = try await request(path: "/graphql", method: "POST", body: .object([
            "query": .string("query NoraMergeSettings($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { nameWithOwner mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed } }"),
            "variables": .object(["owner": .string(parts[0]), "name": .string(parts[1])]),
        ]))
        let settings = response.data["data"]["repository"]
        guard response.data["errors"].isNull || response.data["errors"].array.isEmpty,
              settings["nameWithOwner"].text.caseInsensitiveCompare(repository) == .orderedSame,
              ["mergeCommitAllowed", "squashMergeAllowed", "rebaseMergeAllowed"].allSatisfy({ settings[$0].bool != nil }) else { throw GitHubAPIError.invalidResponse }
        return data.merging(.object([
            "allow_merge_commit": data["allow_merge_commit"].bool.map(GitHubValue.bool) ?? settings["mergeCommitAllowed"],
            "allow_squash_merge": data["allow_squash_merge"].bool.map(GitHubValue.bool) ?? settings["squashMergeAllowed"],
            "allow_rebase_merge": data["allow_rebase_merge"].bool.map(GitHubValue.bool) ?? settings["rebaseMergeAllowed"],
        ]))
    }
}
