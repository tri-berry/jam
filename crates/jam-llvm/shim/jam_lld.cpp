/*
 * Copyright (c) 2026-present Raphael Amorim
 *
 * This file is part of jam.
 * Licensed under the Apache License, Version 2.0 with LLVM Exceptions.
 *
 * The embedded linker. jam links executables in-process with LLD instead of
 * spawning a system compiler driver, so a jam install needs no clang/gcc.
 * Only the Mach-O (ld64.lld) and ELF (ld.lld) drivers are linked in.
 */

#include "lld/Common/Driver.h"

#include "llvm/ADT/ArrayRef.h"
#include "llvm/ADT/StringRef.h"
#include "llvm/Support/raw_ostream.h"

#include <string>

LLD_HAS_DRIVER(elf)
LLD_HAS_DRIVER(macho)

// Run LLD with a full argv. argv[0] selects the driver the same way the
// `lld` binary's name does: "ld64.lld" is Mach-O, "ld.lld" is ELF. Returns
// the linker's exit code; diagnostics go to stderr.
//
// jam may link several programs from one process (`jam test` in serial mode).
// LLD supports that, except after a crash it could not clean up from: it then
// reports `canRunAgain = false`, and every later link in this process fails
// here instead of running on corrupted state.
//
// `quietMinVersion` drops Mach-O's "has version X, which is newer than target
// minimum of Y" warnings. They compare each input's deployment target with the
// program's; when jam picked that target itself (nobody asked for a specific
// OS version) they are noise about how some library happened to be built.
extern "C" int jam_lld_link(int argc, const char *const *argv,
    int quietMinVersion) {
	static bool canRunAgain = true;
	if (!canRunAgain) {
		llvm::errs() << "error: the linker crashed earlier in this process "
		                "and cannot run again\n";
		llvm::errs().flush();
		return 1;
	}
	llvm::ArrayRef<const char *> args(argv, static_cast<size_t>(argc));
	const lld::DriverDef drivers[] = {
	    {lld::Gnu, &lld::elf::link}, {lld::Darwin, &lld::macho::link}};
	lld::Result r;
	if (quietMinVersion) {
		// Collect the diagnostics, then replay every line but those.
		std::string text;
		llvm::raw_string_ostream captured(text);
		captured.enable_colors(llvm::errs().has_colors());
		r = lld::lldMain(args, llvm::outs(), captured, drivers);
		llvm::StringRef rest(text);
		while (!rest.empty()) {
			auto [line, tail] = rest.split('\n');
			if (!line.contains("which is newer than target minimum of"))
				llvm::errs() << line << '\n';
			rest = tail;
		}
	} else {
		r = lld::lldMain(args, llvm::outs(), llvm::errs(), drivers);
	}
	canRunAgain = r.canRunAgain;
	llvm::outs().flush();
	llvm::errs().flush();
	return r.retCode;
}
