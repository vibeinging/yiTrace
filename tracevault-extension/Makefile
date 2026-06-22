# yiTrace 扩展构建/安装(纯 SQL 扩展,无 .so)
# [验] openGauss 的 pg_config / PGXS 路径,若与社区 PG 不同,改用 README 的手工拷贝法。
EXTENSION = tracevault
DATA = tracevault--1.0.sql
PG_CONFIG ?= pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

# 手工安装(PGXS 不可用时):
#   make install-manual PGSHARE=$$($(PG_CONFIG) --sharedir)
install-manual:
	cp tracevault.control "$(PGSHARE)/extension/"
	cp tracevault--1.0.sql "$(PGSHARE)/extension/"
	@echo "已拷贝,登库执行: CREATE EXTENSION tracevault;"
