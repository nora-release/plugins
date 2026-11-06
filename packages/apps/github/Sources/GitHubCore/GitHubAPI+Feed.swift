import Foundation

extension GitHubAPI {
    public func relatedItems(login: String, kind: GitHubKind, repository: String = "", managedRepositories: [String] = [], page: Int = 1) async throws -> GitHubPage {
        guard !login.isEmpty, login.utf8.count <= 100,
              login.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95 }),
              (1...20).contains(page), repository.isEmpty || Self.isValidRepository(repository),
              managedRepositories.allSatisfy(Self.isValidRepository) else { throw GitHubAPIError.invalidRepository }
        var scope = "involves:\(login)"
        if kind == .pullRequests {
            let owned = repository.split(separator: "/").first?.lowercased() == login.lowercased()
            if !repository.isEmpty {
                scope = owned || managedRepositories.contains(where: { $0.caseInsensitiveCompare(repository) == .orderedSame }) ? "" : "(involves:\(login) OR review-involves:\(login))"
            } else {
                scope = "(involves:\(login) OR review-involves:\(login) OR user:\(login)" + managedRepositories.filter { $0.split(separator: "/").first?.lowercased() != login.lowercased() }.map { " OR repo:\($0)" }.joined() + ")"
            }
        }
        let query = [kind == .pullRequests ? "is:pull-request" : "is:issue", "state:open", "archived:false", repository.isEmpty ? "" : "repo:\(repository)", scope].filter { !$0.isEmpty }.joined(separator: " ")
        var components = URLComponents()
        components.queryItems = [URLQueryItem(name: "q", value: query), URLQueryItem(name: "sort", value: "updated"), URLQueryItem(name: "order", value: "desc"), URLQueryItem(name: "per_page", value: "50"), URLQueryItem(name: "page", value: String(page)), URLQueryItem(name: "advanced_search", value: "true")]
        guard let encoded = components.percentEncodedQuery, encoded.utf8.count < 8000 else { throw GitHubAPIError.searchScopeTooLarge }
        let result = try await request(path: "/search/issues?" + encoded)
        guard case .array(let rows) = result.data["items"], result.data["incomplete_results"].bool == false, result.data["total_count"].int != nil else { throw GitHubAPIError.invalidResponse }
        let items = try rows.map { row in
            let name = GitHubService.itemRepository(row)
            guard Self.isValidRepository(name), row["number"].int.map({ $0 > 0 }) == true,
                  repository.isEmpty || name.caseInsensitiveCompare(repository) == .orderedSame else { throw GitHubAPIError.invalidResponse }
            return row.setting("repository", row["repository"].setting("full_name", .string(name)))
        }.filter { $0["state"].text == "open" && $0["merged_at"].isNull && $0["pull_request"]["merged_at"].isNull
            && (!$0["pull_request"].isNull) == (kind == .pullRequests) }
        return GitHubPage(items: items, nextPage: result.hasNext && page < 20 ? page + 1 : nil, isTruncated: (result.data["total_count"].int ?? 0) > 1000 || page == 20 && result.hasNext)
    }
}
