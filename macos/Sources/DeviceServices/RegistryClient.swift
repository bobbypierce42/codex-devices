import Foundation

public struct HTTPRequest: Sendable {
    public let url: URL
    public let method: String
    public let headers: [String: String]
    public let body: Data?
}
public struct HTTPResult: Sendable {
    public let status: Int
    public let data: Data
    public let redirected: Bool
    public init(status: Int, data: Data, redirected: Bool = false) { self.status = status; self.data = data; self.redirected = redirected }
}
public typealias HTTPTransport = @Sendable (HTTPRequest) async throws -> HTTPResult
private final class RejectRedirects: NSObject, URLSessionTaskDelegate, Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
public enum NativeHTTP {
    public static func send(_ request: HTTPRequest) async throws -> HTTPResult {
        guard request.url.scheme == "https", request.url.host == "chatgpt.com", request.url.port == nil, request.url.user == nil, request.url.password == nil else { throw ServiceError.refused }
        let config = URLSessionConfiguration.ephemeral
        config.httpCookieStorage = nil; config.httpShouldSetCookies = false; config.urlCredentialStorage = nil
        config.urlCache = nil; config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.timeoutIntervalForRequest = 15; config.timeoutIntervalForResource = 20
        let session = URLSession(configuration: config, delegate: RejectRedirects(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        var native = URLRequest(url: request.url)
        native.httpMethod = request.method; native.httpBody = request.body
        request.headers.forEach { native.setValue($1, forHTTPHeaderField: $0) }
        do {
            let (stream, response) = try await session.bytes(for: native)
            guard let response = response as? HTTPURLResponse, response.url == request.url else { throw ServiceError.refused }
            guard response.expectedContentLength <= 4 * 1024 * 1024 else { throw ServiceError.oversized }
            var data = Data()
            for try await byte in stream {
                try Task.checkCancellation()
                guard data.count < 4 * 1024 * 1024 else { throw ServiceError.oversized }
                data.append(byte)
            }
            return HTTPResult(status: response.statusCode, data: data)
        } catch let error as ServiceError { throw error }
        catch is CancellationError { throw ServiceError.canceled }
        catch let error as URLError {
            if error.code == .timedOut { throw ServiceError.timeout }
            if error.code == .cancelled { throw ServiceError.canceled }
            throw ServiceError.unavailable
        } catch { throw ServiceError.unavailable }
    }
}
public struct RegistryCapture: Sendable {
    public let accountIDHash: String
    public let rootResponse: String
    public let rootFollowupPages: [RootPage]
    public let scopes: [Scope]
    public struct RootPage: Sendable {
        public let cursor: String
        public let response: String
        public init(cursor: String, response: String) { self.cursor = cursor; self.response = response }
    }
    public struct Scope: Sendable {
        public let dotID: String
        public let threadID: String
        public let computersResponse: String
        public init(dotID: String, threadID: String, computersResponse: String) { self.dotID = dotID; self.threadID = threadID; self.computersResponse = computersResponse }
    }
    public init(accountIDHash: String, rootResponse: String, scopes: [Scope], rootFollowupPages: [RootPage] = []) { self.accountIDHash = accountIDHash; self.rootResponse = rootResponse; self.scopes = scopes; self.rootFollowupPages = rootFollowupPages }
    public var json: JSONValue {
        var fields: [String: JSONValue] = ["schema_version": .number(1), "source": .object(["kind": .string("desktop-aggregate"), "account_id_sha256": .string(accountIDHash)]), "root_response_utf8": .string(rootResponse), "scopes": .array(scopes.map { .object(["dot_id": .string($0.dotID), "thread_id": .string($0.threadID), "computers_response_utf8": .string($0.computersResponse)]) })]
        if !rootFollowupPages.isEmpty { fields["root_followup_pages"] = .array(rootFollowupPages.map { .object(["cursor": .string($0.cursor), "response_utf8": .string($0.response)]) }) }
        return .object(fields)
    }
    public func devices() throws -> [LogicalDevice] {
        guard isHash(accountIDHash), scopes.count <= 4096 else { throw ServiceError.invalidResponse }
        guard rootFollowupPages.count <= 4096 else { throw ServiceError.invalidResponse }
        var expected: String?; var seenCursors = Set<String>(); var items = [JSONValue]()
        let pages = [(nil as String?, rootResponse)] + rootFollowupPages.map { (Optional($0.cursor), $0.response) }
        for (cursor, raw) in pages {
            guard cursor == expected else { throw ServiceError.invalidResponse }
            let root = try JSONValue.parse(Data(raw.utf8))
            guard case .object(let fields) = root, let pageItems = root["items"].array, let next = fields["cursor"] else { throw ServiceError.invalidResponse }
            items += pageItems; guard items.count <= 4096 else { throw ServiceError.oversized }
            if next == .null { expected = nil }
            else {
                guard let value = next.string, !value.isEmpty, value.utf8.count <= 4096, seenCursors.insert(value).inserted else { throw ServiceError.invalidResponse }
                expected = value
            }
        }
        guard expected == nil, items.count == scopes.count else { throw ServiceError.invalidResponse }
        var mapping = [String: String]()
        for item in items {
            guard let dot = item["id"].string, !dot.isEmpty, let thread = item["active_root_thread_id"].string, !thread.isEmpty,
                  item["display_name"].string != nil, mapping.updateValue(thread, forKey: dot) == nil else { throw ServiceError.invalidResponse }
        }
        var records = [String: (JSONValue, String, String, String, String, String, [DeviceOccurrence])]()
        var seenScopes = Set<String>(); var count = 0
        for scope in scopes {
            guard mapping[scope.dotID] == scope.threadID, seenScopes.insert(scope.dotID).inserted else { throw ServiceError.drift }
            let response = try JSONValue.parse(Data(scope.computersResponse.utf8))
            guard case .object(let responseObject) = response, responseObject["next_cursor"] == .null,
                  response["thread_id"].string == scope.threadID, let computers = response["computers"].array else { throw ServiceError.invalidResponse }
            var seen = Set<String>()
            for record in computers {
                count += 1; guard count <= 4096 else { throw ServiceError.oversized }
                guard case .object(let fields) = record, let id = record["environment_id"].string, !id.isEmpty, seen.insert(id).inserted,
                      let kind = record["kind"].string else { throw ServiceError.invalidResponse }
                for field in ["computer_type", "display_name", "status", "installation_id"] {
                    if let value = fields[field], value != .null && value.string == nil { throw ServiceError.invalidResponse }
                }
                // Nonpersonal records remain in the complete capture but cannot be selected.
                let type = record["computer_type"].string ?? "", name = record["display_name"].string ?? ""
                let installation = record["installation_id"].string ?? ""
                let global = JSONValue.array([record["installation_id"], record["kind"], record["computer_type"], record["display_name"], record["status"]])
                let occurrence = DeviceOccurrence(dotID: scope.dotID, threadID: scope.threadID, record: record)
                if var previous = records[id] {
                    guard previous.0 == global else { throw ServiceError.drift }
                    previous.6.append(occurrence); records[id] = previous
                } else { records[id] = (global, installation, kind, type, name, hash(installation), [occurrence]) }
            }
        }
        return records.map { id, row in LogicalDevice(environmentID: id, displayName: row.4, installationIDHash: row.5, kind: row.2, computerType: row.3, occurrences: row.6, installationID: row.1) }.sorted { $0.id < $1.id }
    }
}
public struct RegistryRenameReceipt: Sendable, Equatable {
    public let executorID: String
    public let customDisplayName: String
    public init(executorID: String, customDisplayName: String) {
        self.executorID = executorID; self.customDisplayName = customDisplayName
    }
    public func validates(environmentID: String, newName: String) -> Bool {
        let encoded = Data(executorID.utf8).base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        return !executorID.isEmpty && executorID == executorID.trimmingCharacters(in: .whitespacesAndNewlines)
            && !executorID.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
            && customDisplayName == newName && (environmentID == executorID || environmentID == "ccarenv_b64_" + encoded)
    }
}
public struct RegistryClient: Sendable {
    private let transport: HTTPTransport
    public init(transport: @escaping HTTPTransport = { try await NativeHTTP.send($0) }) { self.transport = transport }
    private func request(account: AccountSession, path: String, query: [URLQueryItem] = [], body: Data? = nil) async throws -> Data {
        var components = URLComponents(); components.scheme = "https"; components.host = "chatgpt.com"; components.path = path; components.queryItems = query.isEmpty ? nil : query
        guard let url = components.url else { throw ServiceError.invalidInput }
        var headers = ["Authorization": "Bearer \(account.token)", "ChatGPT-Account-Id": account.accountID, "Accept": "application/json", "User-Agent": "CodexDevices/0.1"]
        if body != nil { headers["Content-Type"] = "application/json" }
        let result: HTTPResult
        do { result = try await transport(HTTPRequest(url: url, method: body == nil ? "GET" : "POST", headers: headers, body: body)) }
        catch let error as ServiceError { throw error }
        catch is CancellationError { throw ServiceError.canceled }
        catch { throw ServiceError.unavailable }
        guard !result.redirected, !(300...399).contains(result.status) else { throw ServiceError.refused }
        guard result.data.count <= 4 * 1024 * 1024 else { throw ServiceError.oversized }
        guard result.status != 401 && result.status != 403 else { throw ServiceError.authentication }
        guard (200...299).contains(result.status) else { throw ServiceError.unavailable }
        return result.data
    }
    public func capture(account: AccountSession) async throws -> RegistryCapture {
        var cursor: String?; var seenCursors = Set<String>(); var pages = [RegistryCapture.RootPage](); var first = ""; var total = 0; var items = [JSONValue]()
        repeat {
            var query = [URLQueryItem(name: "limit", value: "25"), URLQueryItem(name: "include_room_preview", value: "false")]
            if let cursor { query.append(URLQueryItem(name: "cursor", value: cursor)) }
            let data = try await request(account: account, path: "/backend-api/tbo", query: query)
            total += data.count; guard total <= 4 * 1024 * 1024, let raw = String(data: data, encoding: .utf8) else { throw ServiceError.oversized }
            if let cursor { pages.append(.init(cursor: cursor, response: raw)) } else { first = raw }
            let root = try JSONValue.parse(data)
            guard case .object(let fields) = root, let pageItems = root["items"].array, let next = fields["cursor"] else { throw ServiceError.invalidResponse }
            items += pageItems; guard items.count <= 4096, pages.count <= 4096 else { throw ServiceError.oversized }
            if next == .null { cursor = nil }
            else {
                guard let value = next.string, !value.isEmpty, value.utf8.count <= 4096, seenCursors.insert(value).inserted else { throw ServiceError.invalidResponse }
                cursor = value
            }
        } while cursor != nil
        var scopes = [RegistryCapture.Scope](); var seen = Set<String>()
        for item in items {
            guard let dot = item["id"].string, !dot.isEmpty, !dot.contains("/"), !dot.contains("?"), !dot.contains("#"), dot != ".", dot != "..", !dot.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains), seen.insert(dot).inserted,
                  let thread = item["active_root_thread_id"].string, !thread.isEmpty, item["display_name"].string != nil else { throw ServiceError.invalidResponse }
            let computers = try await request(account: account, path: "/backend-api/tbo/\(dot)/computers", query: [URLQueryItem(name: "expected_thread_id", value: thread)])
            total += computers.count; guard total <= 4 * 1024 * 1024, let text = String(data: computers, encoding: .utf8) else { throw ServiceError.oversized }
            scopes.append(.init(dotID: dot, threadID: thread, computersResponse: text))
        }
        let capture = RegistryCapture(accountIDHash: account.accountIDHash, rootResponse: first, scopes: scopes, rootFollowupPages: pages)
        _ = try capture.devices()
        return capture
    }
    @discardableResult public func rename(account: AccountSession, installationID: String, newName: String, expectedEnvironmentID: String? = nil) async throws -> RegistryRenameReceipt {
        guard !installationID.isEmpty, !newName.isEmpty, newName.utf16.count <= 100, newName == newName.trimmingCharacters(in: .whitespacesAndNewlines), !newName.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw ServiceError.invalidInput }
        let body = try JSONValue.object(["installation_id": .string(installationID), "custom_display_name": .string(newName)]).data()
        let response = try JSONValue.parse(await request(account: account, path: "/backend-api/flora/cca/executor", body: body))
        guard let executor = response["executor_id"].string, !executor.isEmpty, executor == executor.trimmingCharacters(in: .whitespacesAndNewlines), !executor.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
              response["custom_display_name"].string == newName else { throw ServiceError.invalidResponse }
        let receipt = RegistryRenameReceipt(executorID: executor, customDisplayName: newName)
        if let expectedEnvironmentID, !receipt.validates(environmentID: expectedEnvironmentID, newName: newName) { throw ServiceError.drift }
        return receipt
    }
}
