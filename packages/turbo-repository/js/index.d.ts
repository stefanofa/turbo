export class Repository {
  readonly root: string;
  readonly isMonorepo: boolean;
  static detectJS(path?: string | undefined | null): Repository;
  packageManager(): PackageManager;
  workspaceDirectories(): Array<string>;
}
export class PackageManager {
  name: string;
}
