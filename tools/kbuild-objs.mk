# Evaluate a Linux kbuild Makefile with RustOS's configuration and print
# what it builds (used by tools/kbuild-group.py). Variables:
#   LINUX   Linux tree       CONFIG  .config      SRC  directory of the Makefile/Kbuild
#   OBJ     the object variable to print (e.g. amdgpu-y)
include $(LINUX)/scripts/Kbuild.include
include $(CONFIG)
srctree := $(LINUX)
src := $(SRC)
obj := $(SRC)
CC_FLAGS_FPU := -msse -msse2
CC_FLAGS_NO_FPU := -mno-sse -mno-sse2
# Newer drivers (nouveau) keep their rules in Kbuild.
ifneq ($(wildcard $(SRC)/Kbuild),)
include $(SRC)/Kbuild
else
include $(SRC)/Makefile
endif

.PHONY: print
print:
	@echo 'OBJS $($(OBJ))'
	@echo 'CCFLAGS $(ccflags-y) $(subdir-ccflags-y)'
	@$(foreach v,$(filter CFLAGS_% CFLAGS_REMOVE_%,$(.VARIABLES)),echo '$(v) $($(v))';)
