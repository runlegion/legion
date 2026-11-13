#!/bin/bash
D=/Volumes/store/projects/runlegion/legion-wt/toy-tests/evidence/test-rule-toy
run() { id=$1; shift; docs="$*"
  msg="$(cat $D/prompt.md)

---

Your inputs (read them; read nothing else):
- the requirement's statement: $D/in/stmt-$id.json
- the intent it serves: $D/in/intent.json
- the documents its traces_to names:"
  for f in $docs; do msg="$msg
  - $D/in/$f"; done
  msg="$msg

Write your JSON to: $D/out/out-$id.json"
  cd $D && claude -p "$msg" --allowedTools "Read,Write" --add-dir $D > $D/out/log-$id.txt 2>&1
}
run FR-MAIL-049 doc-01a1a4c4-186c-7021-9c10-159f4f377b50.json doc-01a1ae1d-a20d-7501-9fd8-c4ed9598b66e.json reflection-01a1a48e.txt &
run FR-MAIL-004 doc-01a18fb8-4127-7391-9883-bb022b45bd75.json doc-01a19b46-cbfb-7702-999b-841607c44bc0.json doc-01a19b44-5fe5-7342-bb90-85b91511d33d.json &
run FR-MAIL-050 doc-01a1a4c4-186c-7021-9c10-159f4f377b50.json doc-01a1ae1d-a20d-7501-9fd8-c4ed9598b66e.json reflection-01a1a48e.txt &
run FR-MAIL-034 doc-01a19d7e-fd4e-76e2-b4ed-c31f922020e7.json doc-01a19daa-fafd-7a43-bd03-3fcb69311297.json doc-01a1a4ad-f543-7221-b4d4-0da138e97728.json &
wait
echo done
