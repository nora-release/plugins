//go:build darwin

package telemetry

/*
#include <mach/mach.h>
#include <mach/thread_info.h>
#include <pthread.h>
#include <stdlib.h>

typedef struct {
	uint64_t thread_id;
	int cpu_usage;
	uint64_t user_time_usec;
	uint64_t system_time_usec;
	int run_state;
	char name[64];
} cue_thread_sample_t;

static int cue_sample_threads(cue_thread_sample_t **out_samples, int *out_count) {
	thread_act_array_t thread_list;
	mach_msg_type_number_t thread_count;
	kern_return_t kr = task_threads(mach_task_self(), &thread_list, &thread_count);
	if (kr != KERN_SUCCESS) {
		return (int)kr;
	}

	cue_thread_sample_t *samples = calloc(thread_count, sizeof(cue_thread_sample_t));
	if (!samples) {
		vm_deallocate(mach_task_self(), (vm_address_t)thread_list, thread_count * sizeof(thread_t));
		return -1;
	}

	for (mach_msg_type_number_t i = 0; i < thread_count; i++) {
		thread_t thr = thread_list[i];
		cue_thread_sample_t *s = &samples[i];

		thread_basic_info_data_t info;
		mach_msg_type_number_t info_count = THREAD_BASIC_INFO_COUNT;
		kr = thread_info(thr, THREAD_BASIC_INFO, (thread_info_t)&info, &info_count);
		if (kr == KERN_SUCCESS) {
			s->cpu_usage = info.cpu_usage;
			s->user_time_usec = ((uint64_t)info.user_time.seconds * 1000000ULL) + (uint64_t)info.user_time.microseconds;
			s->system_time_usec = ((uint64_t)info.system_time.seconds * 1000000ULL) + (uint64_t)info.system_time.microseconds;
			s->run_state = info.run_state;
		}

		thread_identifier_info_data_t idinfo;
		mach_msg_type_number_t idinfo_count = THREAD_IDENTIFIER_INFO_COUNT;
		kr = thread_info(thr, THREAD_IDENTIFIER_INFO, (thread_info_t)&idinfo, &idinfo_count);
		if (kr == KERN_SUCCESS) {
			s->thread_id = idinfo.thread_id;
		}

		pthread_t pthr = pthread_from_mach_thread_np(thr);
		if (pthr) {
			pthread_getname_np(pthr, s->name, sizeof(s->name));
		}
	}

	vm_deallocate(mach_task_self(), (vm_address_t)thread_list, thread_count * sizeof(thread_t));

	*out_samples = samples;
	*out_count = (int)thread_count;
	return 0;
}

static void cue_free_samples(cue_thread_sample_t *samples) {
	free(samples);
}
*/
import "C"

import (
	"fmt"
	"unsafe"
)

type hostThreadSample struct {
	ThreadID       uint64
	Name           string
	CPUUsageScaled int
	UserTimeUsec   uint64
	SystemTimeUsec uint64
	RunState       int
}

type hostThreadSampler struct{}

func (s hostThreadSampler) Sample() ([]hostThreadSample, error) {
	var samples *C.cue_thread_sample_t
	var count C.int
	rc := C.cue_sample_threads(&samples, &count)
	if rc != 0 {
		return nil, fmt.Errorf("sample threads: %d", int(rc))
	}
	if samples == nil || count <= 0 {
		return nil, nil
	}
	defer C.cue_free_samples(samples)

	out := make([]hostThreadSample, 0, int(count))
	slice := unsafe.Slice(samples, int(count))
	for _, item := range slice {
		name := C.GoString(&item.name[0])
		out = append(out, hostThreadSample{
			ThreadID:       uint64(item.thread_id),
			Name:           name,
			CPUUsageScaled: int(item.cpu_usage),
			UserTimeUsec:   uint64(item.user_time_usec),
			SystemTimeUsec: uint64(item.system_time_usec),
			RunState:       int(item.run_state),
		})
	}
	return out, nil
}

func cpuPercentFromScaledUsage(usageScaled int) float64 {
	if usageScaled <= 0 {
		return 0
	}
	return float64(usageScaled) * 100.0 / float64(C.TH_USAGE_SCALE)
}
