# M0 private verification-store parent

Purpose: remove the shared-temporary-parent assumption before any future cleanup design. Create a fresh exclusive0700parent beneath a pinned canonical temporary anchor, then a fixed private store child. Validate both private levels UID/mode and pinned identities before inspection/journal mutation; synchronize created entries. No path inputs, deletion, adoption, new childcommands or renderer authority. All fixtures retained, including partial creation.

Parent reviews concrete code before any new fixture tests. Test actual private modes and invalidatedparent refusal; adapt differentrootidentity coverage to fixedrootname. Keepworkerbinding/receipts unchanged. Version.17; branchfromPR28head6a5435e; preservepending27integrationnote, current28CIwatchnotpolled. Finalintegrationmain afterbaseline.
