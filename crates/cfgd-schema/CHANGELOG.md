# Changelog — cfgd-schema

## [0.5.0] - 2026-09-16

### Features

* 1039747a5b87 let a profile pin a backup unit's schedule to the machine with scheduleOwner, and name the owning layer in backup list ([@tj-smith47](https://github.com/tj-smith47))
* d89de00d4753 widen the Module CRD to the local kind — a package entry carries its per-manager `aliases`, its `deny` list and its own platform gates, every platform tag is validated by the rule the local parser refuses one by, and `spec.files` refuses an empty or duplicated server-side-apply key before the API server rejects the whole resource naming neither entry ([@tj-smith47](https://github.com/tj-smith47))
* e840ed29cca4 carry a device's package versions and backup schedule owners onto its MachineConfig status at check-in, and answer with the BackupPolicy schedules the cluster owns so a fleet cadence reaches the machine ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* fa660aa308e0 read a cluster-projected cadence as projected in the Schedule Owner column, leaving the stored spelling and -o json at the layer the profile declared ([@tj-smith47](https://github.com/tj-smith47))
* 84e3219a8b0a name the backup list's schedule-owner column after its field, spell the enum as the schema does in the docs, and pin the Local arm through the listing ([@tj-smith47](https://github.com/tj-smith47))
* a264a08e8c22 render a changed post-apply script on the module upgrade screen through the one Scripts composer ([@tj-smith47](https://github.com/tj-smith47))
* 1dce720521b4 state every knob a module's script step declares above its body, so the upgrade screen cannot report a step as changed with nothing to show ([@tj-smith47](https://github.com/tj-smith47))
* 4ef5cd2eba8a refuse a leading-dash package name and suggest the @version spelling ([@tj-smith47](https://github.com/tj-smith47))
* 6f63e9b9ca0d refuse a package name a Windows shim would read as a second command ([@tj-smith47](https://github.com/tj-smith47))
* 4d1c77e2abcb word a module file declared twice as declared twice, never as duplicating itself ([@tj-smith47](https://github.com/tj-smith47))
* cf219df281e4 refuse a declared script step with nothing to run, naming the hook and the step index ([@tj-smith47](https://github.com/tj-smith47))
* 54821bf47c61 report a post-apply step whose timeout or guard moved as a change on the module upgrade screen ([@tj-smith47](https://github.com/tj-smith47))
* ab6d25e549b1 name both claimants when two module file entries share a target, and word each refusal with the one subject its parser uses ([@tj-smith47](https://github.com/tj-smith47))
* 83aae4f105c0 refuse a blank postApply command on a Module, and make the pod webhook read blank as absent ([@tj-smith47](https://github.com/tj-smith47))
* d2135a9ffde0 refuse a blank postApply in the words of the path it judges, and document the field as a path ([@tj-smith47](https://github.com/tj-smith47))
* 97818ebfa541 validate a BackupPolicy unit's schedule and name by the grammar the machine parses, hoisted into cfgd-schema beside the local parser, and make the CRD completeness guard read every roster a kind must join ([@tj-smith47](https://github.com/tj-smith47))
* 550f8a49964d read a machine's schedule-owner pin through the one parser and never apply on an unreadable word, count the Applied condition by distinct units and machines, refuse a BackupPolicy that schedules nothing, and drop the accessor nothing calls ([@tj-smith47](https://github.com/tj-smith47))
* 41947ea0e9a3 name the Brewfile list a refused manifest package name came from ([@tj-smith47](https://github.com/tj-smith47))
* 1b645cf707bf make the CSI dependency gate fail when cargo tree resolves nothing, declare cfgd-core's publish edge to cfgd-schema, type the file-shape refusal, and import the case-insensitive enum macro at each call site ([@tj-smith47](https://github.com/tj-smith47))
* acb6904b048f answer a backup unit's duplicate-name and zero-retention refusals from one shared rule, so a profile and a BackupPolicy cannot disagree about the shape of a unit ([@tj-smith47](https://github.com/tj-smith47))
* ed834b401528 extract the shared config value types into a leaf cfgd-schema crate so the local parser and the CRD schema define a file's patch shape once ([@tj-smith47](https://github.com/tj-smith47))
