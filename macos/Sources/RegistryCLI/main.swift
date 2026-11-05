import Foundation
import Darwin
import DeviceServices

@main struct RegistryCLI {
    static func main() async {
        umask(0o077)
        var limit = rlimit(rlim_cur: 0, rlim_max: 0)
        guard setrlimit(RLIMIT_CORE, &limit) == 0 else {
            print(RegistryCommandRunner.failure(.refused, uncertain: false).output); exit(3)
        }
        let result: RegistryCommandResult
        do {
            let command = try RegistryCommand.parse(Array(CommandLine.arguments.dropFirst()))
            let executable = Bundle.main.executableURL ?? URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
            let providers = RegistryCommandProviders.native(command: command, executableURL: executable)
            result = await RegistryCommandRunner(providers: providers).run(command)
        } catch { result = RegistryCommandRunner.failure(.invalidInput, uncertain: false) }
        print(result.output)
        exit(result.exitCode)
    }
}
