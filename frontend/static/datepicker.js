/**
 * Simple Date Picker Component
 * Adds date filtering functionality to the meetings list
 */

const DatePicker = {
    filterDate: localStorage.getItem('filterDate') || '',
    onChange: null,

    init(inputId, onChange) {
        this.onChange = onChange;
        const input = document.getElementById(inputId);
        if (!input) return;

        // Restore saved date
        input.value = this.filterDate;

        // Handle change
        input.addEventListener('change', (e) => {
            this.filterDate = e.target.value;
            localStorage.setItem('filterDate', this.filterDate);
            if (this.onChange) this.onChange(this.filterDate);
        });

        // Clear button on double-click
        input.addEventListener('dblclick', () => {
            input.value = '';
            this.filterDate = '';
            localStorage.setItem('filterDate', '');
            if (this.onChange) this.onChange('');
        });
    },

    getDate() {
        return this.filterDate;
    },

    // Filter meetings by date (YYYY-MM-DD format)
    filterMeetings(meetings, dateStr) {
        if (!dateStr) return meetings;

        return meetings.filter(m => {
            const createdAt = m.created_at;
            if (!createdAt) return false;
            // created_at is ISO format, extract date part
            const meetingDate = createdAt.split('T')[0];
            return meetingDate === dateStr;
        });
    }
};

// Export for use in app.js
window.DatePicker = DatePicker;
